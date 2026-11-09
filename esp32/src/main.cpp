// ESP32 "body" for the companion app.
//
// - Loops an emotion GIF on the left of a 320x240 ILI9341 screen.
// - Connects to the Tauri app's WebSocket server and listens for
//   {"emotion": "...", "text": "..."} messages, then shows the text on the right
//   and switches to that emotion's GIF.
//
// GIFs are compiled into flash for now (see tools/gif2header.py). To serve them
// from the app later, only gifForEmotion()/startGif() need to change.

#include <Arduino.h>
#include <SPI.h>
#include <WiFi.h>
#include <Adafruit_GFX.h>
#include <Adafruit_ILI9341.h>
#include <AnimatedGIF.h>
#include <WebSocketsClient.h>
#include <ArduinoJson.h>

#include "gifs/wave.h"

// ---- Config ----
// Wokwi's simulated Wi-Fi. On real hardware, put your own network here.
static const char *WIFI_SSID = "Wokwi-GUEST";
static const char *WIFI_PASS = "";
static const int WIFI_CHANNEL = 6;  // Wokwi-GUEST is on channel 6; skips the scan

// In the Wokwi simulator, host.wokwi.internal is the PC running the simulator.
// On real hardware, use the PC's LAN IP instead.
static const char *WS_HOST = "host.wokwi.internal";
static const uint16_t WS_PORT = 8080;

// ---- Display layout (landscape, 320x240) ----
#define TFT_DC 2
#define TFT_CS 15
#define TFT_RST 4

static const int SCREEN_W = 320;
static const int SCREEN_H = 240;
static const int GIF_AREA_W = 200;               // left: animation
static const int PANEL_X = GIF_AREA_W + 4;       // right: emotion + reply text
static const int PANEL_W = SCREEN_W - PANEL_X - 2;
static const int STATUS_H = 12;                  // bottom of the panel: connection status
static const int CHAR_W = 6, CHAR_H = 8;         // default GFX font at text size 1

Adafruit_ILI9341 tft(TFT_CS, TFT_DC, TFT_RST);
AnimatedGIF gif;
WebSocketsClient ws;

// ---- Emotion -> GIF table ----
struct EmotionGif {
  const char *emotion;
  const uint8_t *data;
  size_t len;
};

// Only one animation exists so far, so every emotion uses it. Add a header per
// emotion with tools/gif2header.py and point the matching row at it.
static const EmotionGif EMOTION_GIFS[] = {
    {"neutral", gif_wave, gif_wave_len},
    {"happy", gif_wave, gif_wave_len},
    {"excited", gif_wave, gif_wave_len},
    {"sad", gif_wave, gif_wave_len},
    {"thinking", gif_wave, gif_wave_len},
};

const EmotionGif *gifForEmotion(const String &emotion) {
  for (const auto &g : EMOTION_GIFS) {
    if (emotion.equalsIgnoreCase(g.emotion)) return &g;
  }
  return &EMOTION_GIFS[0];
}

// ---- GIF playback ----
static const EmotionGif *currentGif = nullptr;
static bool gifOpen = false;
static int gifX = 0, gifY = 0;  // top-left so the GIF is centered in its area
static uint32_t nextFrameAt = 0;
static uint16_t lineBuf[SCREEN_W];

// Called by AnimatedGIF once per decoded line. Pixels are palette indices.
void GIFDraw(GIFDRAW *pDraw) {
  int y = gifY + pDraw->iY + pDraw->y;
  int x = gifX + pDraw->iX;
  int w = pDraw->iWidth;
  if (y < 0 || y >= SCREEN_H) return;
  if (x + w > GIF_AREA_W) w = GIF_AREA_W - x;
  if (w <= 0) return;

  uint8_t *s = pDraw->pPixels;
  uint16_t *pal = pDraw->pPalette;

  // "Restore to background" disposal: paint transparent pixels with the background
  if (pDraw->ucDisposalMethod == 2) {
    for (int i = 0; i < w; i++) {
      if (s[i] == pDraw->ucTransparent) s[i] = pDraw->ucBackground;
    }
    pDraw->ucHasTransparency = 0;
  }

  tft.startWrite();
  if (pDraw->ucHasTransparency) {
    // Draw only the runs of opaque pixels, leaving transparent ones untouched
    uint8_t t = pDraw->ucTransparent;
    int i = 0;
    while (i < w) {
      while (i < w && s[i] == t) i++;
      int start = i, n = 0;
      while (i < w && s[i] != t) lineBuf[n++] = pal[s[i++]];
      if (n) {
        tft.setAddrWindow(x + start, y, n, 1);
        tft.writePixels(lineBuf, n);
      }
    }
  } else {
    for (int i = 0; i < w; i++) lineBuf[i] = pal[s[i]];
    tft.setAddrWindow(x, y, w, 1);
    tft.writePixels(lineBuf, w);
  }
  tft.endWrite();
}

void startGif(const EmotionGif *g) {
  if (g == currentGif && gifOpen) return;  // already playing this one
  if (gifOpen) gif.close();
  currentGif = g;
  gifOpen = gif.open((uint8_t *)g->data, g->len, GIFDraw);
  if (!gifOpen) {
    Serial.printf("Failed to open GIF for '%s' (error %d)\n", g->emotion, gif.getLastError());
    return;
  }
  gifX = max(0, (GIF_AREA_W - gif.getCanvasWidth()) / 2);
  gifY = max(0, (SCREEN_H - gif.getCanvasHeight()) / 2);
  tft.fillRect(0, 0, GIF_AREA_W, SCREEN_H, ILI9341_BLACK);
  nextFrameAt = 0;
}

// Draw at most one frame per call so the WebSocket keeps getting serviced
void tickGif() {
  if (!gifOpen || millis() < nextFrameAt) return;
  int delayMs = 0;
  int more = gif.playFrame(false, &delayMs);
  if (more <= 0) gif.reset();  // last frame (or error): loop from the start
  nextFrameAt = millis() + max(delayMs, 20);
}

// ---- Text panel ----

// The GFX font only covers ASCII, so drop emoji and other non-ASCII characters
String toAscii(const String &in) {
  String out;
  out.reserve(in.length());
  for (size_t i = 0; i < in.length(); i++) {
    char c = in[i];
    if (c == '\n' || (c >= 32 && c < 127)) out += c;
  }
  return out;
}

// Word-wrap text into the panel, clipping at maxY
void drawWrapped(const String &text, int x, int y, int maxY) {
  const int cols = PANEL_W / CHAR_W;
  String line, word;
  auto flush = [&](String &l) {
    if (y + CHAR_H > maxY) return;
    tft.setCursor(x, y);
    tft.print(l);
    y += CHAR_H + 2;
    l = "";
  };
  for (size_t i = 0; i <= text.length(); i++) {
    char c = i < text.length() ? text[i] : ' ';
    if (c != ' ' && c != '\n') {
      word += c;
      // Hard-break words longer than a full line
      if ((int)word.length() == cols) {
        if (line.length()) flush(line);
        flush(word);
      }
      continue;
    }
    if (word.length()) {
      int needed = line.length() ? line.length() + 1 + word.length() : word.length();
      if (needed > cols) flush(line);
      if (line.length()) line += ' ';
      line += word;
      word = "";
    }
    if (c == '\n' && line.length()) flush(line);
  }
  if (line.length()) flush(line);
}

void showReply(const String &emotion, const String &text) {
  tft.fillRect(PANEL_X, 0, SCREEN_W - PANEL_X, SCREEN_H - STATUS_H, ILI9341_BLACK);
  tft.setTextSize(2);
  tft.setTextColor(ILI9341_PINK);
  tft.setCursor(PANEL_X, 4);
  tft.print(emotion);
  tft.setTextSize(1);
  tft.setTextColor(ILI9341_WHITE);
  drawWrapped(toAscii(text), PANEL_X, 28, SCREEN_H - STATUS_H - 2);
}

void showStatus(const char *msg, uint16_t color) {
  tft.fillRect(PANEL_X, SCREEN_H - STATUS_H, SCREEN_W - PANEL_X, STATUS_H, ILI9341_BLACK);
  tft.setTextSize(1);
  tft.setTextColor(color);
  tft.setCursor(PANEL_X, SCREEN_H - STATUS_H + 2);
  tft.print(msg);
}

// ---- WebSocket ----
void onWsEvent(WStype_t type, uint8_t *payload, size_t length) {
  switch (type) {
    case WStype_CONNECTED:
      Serial.println("WebSocket connected");
      showStatus("App linked", ILI9341_GREEN);
      break;
    case WStype_DISCONNECTED:
      Serial.println("WebSocket disconnected");
      showStatus("App offline", ILI9341_RED);
      break;
    case WStype_TEXT: {
      Serial.printf("Got: %.*s\n", (int)length, (const char *)payload);
      JsonDocument doc;
      if (deserializeJson(doc, payload, length)) {
        Serial.println("Ignoring non-JSON message");
        return;
      }
      String emotion = doc["emotion"] | "neutral";
      String text = doc["text"] | "";
      startGif(gifForEmotion(emotion));
      showReply(emotion, text);
      break;
    }
    default:
      break;
  }
}

// ---- Main ----
static bool wsStarted = false;

void setup() {
  Serial.begin(115200);
  Serial.println("Booting...");

  tft.begin();
  tft.setRotation(1);
  tft.fillScreen(ILI9341_BLACK);

  gif.begin(GIF_PALETTE_RGB565_LE);
  startGif(gifForEmotion("neutral"));
  showReply("hello", "Waiting for the companion app...");
  showStatus("WiFi...", ILI9341_YELLOW);

  // Connect in the background so the animation keeps playing during boot
  WiFi.begin(WIFI_SSID, WIFI_PASS, WIFI_CHANNEL);
}

void loop() {
  if (!wsStarted && WiFi.status() == WL_CONNECTED) {
    Serial.printf("WiFi connected, IP %s. Connecting to ws://%s:%u\n",
                  WiFi.localIP().toString().c_str(), WS_HOST, WS_PORT);
    showStatus("Finding app...", ILI9341_YELLOW);
    ws.begin(WS_HOST, WS_PORT, "/");
    ws.onEvent(onWsEvent);
    ws.setReconnectInterval(3000);  // keeps retrying if the app isn't running yet
    wsStarted = true;
  }
  if (wsStarted) ws.loop();
  tickGif();
}
