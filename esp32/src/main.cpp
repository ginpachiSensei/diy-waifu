// ESP32 "body" for the companion app.
//
// - Plays the avatar GIF on the left of a 320x240 ILI9341 screen and shows the
//   reply text on the right.
// - Connects to the app's WebSocket, says hello as "esp32", and follows the same
//   avatar states as the pet and the phone (phone/avatar.js): the reply's emotion
//   for 8s, then talking / searching / thinking / sleeping (away) / the user's
//   activity / idle.
// - GIFs come from the app (http://<pc>:8081/esp32/...), already sized for this
//   screen, and are cached in flash (LittleFS) by id. New art shows up the next
//   time it connects, with no reflash. The compiled-in wave is the fallback.
//
// Real hardware (env esp32dev): on first boot it opens a Wi-Fi hotspot,
// "Companion-Setup". Join it, pick your Wi-Fi, and enter the pairing code from the
// app's Bodies tab. Leave "PC address" blank to find the app over mDNS
// (diy-waifu.local). Hold the BOOT button for 3s to forget the settings.
//
// Wokwi simulator (env wokwi, -DWOKWI): fixed Wokwi-GUEST Wi-Fi and
// host.wokwi.internal, which reaches the app as localhost (no code needed).

#include <Arduino.h>
#include <SPI.h>
#include <WiFi.h>
#include <HTTPClient.h>
#include <LittleFS.h>
#include <ESPmDNS.h>
#include <Preferences.h>
#include <Adafruit_GFX.h>
#include <Adafruit_ILI9341.h>
#include <AnimatedGIF.h>
#include <WebSocketsClient.h>
#include <ArduinoJson.h>
#ifndef WOKWI
#include <WiFiManager.h>
#endif

#include "gifs/wave.h"

// ---- Config ----
static const uint16_t WS_PORT = 8080;
static const uint16_t HTTP_PORT = 8081;
static const int RESET_BUTTON = 0;  // the BOOT button
static const uint32_t REPLY_HOLD_MS = 8000;

#ifdef WOKWI
static const char *WIFI_SSID = "Wokwi-GUEST";
static const int WIFI_CHANNEL = 6;  // skips the scan
static const char *FIXED_HOST = "host.wokwi.internal";
#endif

// ---- Display layout (landscape, 320x240) ----
#define TFT_DC 2
#define TFT_CS 15
#define TFT_RST 4

static const int SCREEN_W = 320;
static const int SCREEN_H = 240;
static const int GIF_AREA_W = 200;          // left: animation
static const int PANEL_X = GIF_AREA_W + 4;  // right: emotion + reply text
static const int PANEL_W = SCREEN_W - PANEL_X - 2;
static const int STATUS_H = 12;             // bottom of the panel: connection status
static const int CHAR_W = 6, CHAR_H = 8;    // default GFX font at text size 1

Adafruit_ILI9341 tft(TFT_CS, TFT_DC, TFT_RST);
AnimatedGIF gif;
WebSocketsClient ws;
Preferences prefs;
#ifndef WOKWI
WiFiManager wm;
WiFiManagerParameter tokenParam("token", "Pairing code (app: Bodies tab)", "", 12);
WiFiManagerParameter hostParam("host", "PC address (blank: find it automatically)", "", 64);
#endif

String cfgToken, cfgHost;  // saved settings
String pcHost;             // where the app is, once found
String wsPath;             // "/?token=..." (must outlive ws.begin)

// ---------------------------------------------------------------------------
// Text panel
// ---------------------------------------------------------------------------

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
      if ((int)word.length() == cols) {  // hard-break words longer than a line
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

void showReply(const String &title, const String &text) {
  tft.fillRect(PANEL_X, 0, SCREEN_W - PANEL_X, SCREEN_H - STATUS_H, ILI9341_BLACK);
  tft.setTextSize(2);
  tft.setTextColor(ILI9341_PINK);
  tft.setCursor(PANEL_X, 4);
  tft.print(title);
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

// ---------------------------------------------------------------------------
// GIFs from the app, cached in LittleFS
// ---------------------------------------------------------------------------

static const int MAX_STATES = 24;
struct StateGif {
  String state;
  String path;  // "/g/<id>.gif" in LittleFS, empty if it couldn't be stored
};
static StateGif stateGifs[MAX_STATES];
static int stateCount = 0;
static String defaultPath;
static bool fsReady = false;

// Same fallbacks as gifFor() in phone/avatar.js
const char *fallbackOf(const String &s) {
  if (s == "excited") return "happy";
  if (s == "searching") return "thinking";
  return "idle";
}

// LittleFS path for a state (following fallbacks), or "" for the built-in GIF
String pathForState(String s) {
  for (int hops = 0; hops < 6; hops++) {
    for (int i = 0; i < stateCount; i++) {
      if (stateGifs[i].state == s && stateGifs[i].path.length()) return stateGifs[i].path;
    }
    if (s == "idle") break;
    s = fallbackOf(s);
  }
  return defaultPath;
}

size_t freeBytes() { return LittleFS.totalBytes() - LittleFS.usedBytes(); }

// Deletes cached GIFs the manifest no longer names
void pruneCache(JsonObject files) {
  File dir = LittleFS.open("/g");
  if (!dir) return;
  String stale[16];
  int n = 0;
  for (File f = dir.openNextFile(); f && n < 16; f = dir.openNextFile()) {
    String name = String("/g/") + f.name();
    bool keep = false;
    for (JsonPair kv : files) {
      if (name == String("/g/") + (const char *)(kv.value()["id"] | "") + ".gif") keep = true;
    }
    if (!keep) stale[n++] = name;
  }
  dir.close();
  for (int i = 0; i < n; i++) LittleFS.remove(stale[i]);
}

bool download(const String &url, const String &path, size_t size) {
  if (freeBytes() < size + 8192) {
    Serial.printf("No room for %s (%u bytes, %u free)\n", path.c_str(), (unsigned)size, (unsigned)freeBytes());
    return false;
  }
  HTTPClient http;
  http.setTimeout(30000);  // the app converts a GIF on first request
  if (!http.begin(url)) return false;
  int code = http.GET();
  bool ok = false;
  if (code == 200) {
    String tmp = path + ".part";
    File f = LittleFS.open(tmp, "w");
    if (f) {
      int written = http.writeToStream(&f);
      f.close();
      ok = written > 0 && (size_t)written == size && LittleFS.rename(tmp, path);
      if (!ok) LittleFS.remove(tmp);
    }
  }
  http.end();
  Serial.printf("GIF %s -> %s: %s\n", url.c_str(), path.c_str(), ok ? "ok" : "failed");
  return ok;
}

void closeGif();

// Fetches the manifest and any GIFs not cached yet. Blocks for a few seconds
// the first time; later connects only check the manifest.
void syncGifs() {
  if (!fsReady) return;
  HTTPClient http;
  http.setTimeout(30000);
  String base = "http://" + pcHost + ":" + HTTP_PORT;
  if (!http.begin(base + "/esp32/manifest.json")) return;
  int code = http.GET();
  if (code != 200) {
    Serial.printf("Manifest: HTTP %d\n", code);
    http.end();
    return;
  }
  JsonDocument doc;
  DeserializationError err = deserializeJson(doc, http.getString());
  http.end();
  if (err) return;

  JsonObject files = doc["files"];
  closeGif();  // a file being played could be pruned
  pruneCache(files);
  LittleFS.mkdir("/g");

  auto ensure = [&](const char *file) -> String {
    if (!file || !*file) return "";
    JsonObject info = files[file];
    if (info.isNull()) return "";
    String path = String("/g/") + (const char *)(info["id"] | "x") + ".gif";
    if (LittleFS.exists(path)) return path;
    showStatus("Getting art...", ILI9341_YELLOW);
    return download(base + "/esp32/gifs/" + file, path, info["size"] | 0) ? path : String();
  };

  defaultPath = ensure(doc["default"] | "");
  stateCount = 0;
  for (JsonPair kv : doc["states"].as<JsonObject>()) {
    if (stateCount >= MAX_STATES) break;
    stateGifs[stateCount++] = {String(kv.key().c_str()), ensure(kv.value().as<const char *>())};
  }
  Serial.printf("Art ready: %d states, %u bytes free\n", stateCount, (unsigned)freeBytes());
}

// ---------------------------------------------------------------------------
// GIF playback (from LittleFS, or the compiled-in fallback)
// ---------------------------------------------------------------------------

static File gifFile;
static String wantedPath = "-";  // what was asked for ("" means the built-in GIF)
static bool gifOpen = false;
static int gifX = 0, gifY = 0;  // top-left so the GIF is centered in its area
static uint32_t nextFrameAt = 0;
static uint16_t lineBuf[SCREEN_W];

void *gifOpenFile(const char *name, int32_t *size) {
  gifFile = LittleFS.open(name, "r");
  if (!gifFile) return nullptr;
  *size = gifFile.size();
  return &gifFile;
}
void gifCloseFile(void *handle) { static_cast<File *>(handle)->close(); }
int32_t gifReadFile(GIFFILE *g, uint8_t *buf, int32_t len) {
  File *f = static_cast<File *>(g->fHandle);
  int32_t n = min(len, g->iSize - g->iPos);
  if (n <= 0) return 0;
  n = f->read(buf, n);
  g->iPos = f->position();
  return n;
}
int32_t gifSeekFile(GIFFILE *g, int32_t pos) {
  File *f = static_cast<File *>(g->fHandle);
  f->seek(pos);
  g->iPos = f->position();
  return g->iPos;
}

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

void closeGif() {
  if (gifOpen) gif.close();
  gifOpen = false;
  wantedPath = "-";
}

void playPath(const String &path) {
  if (path == wantedPath && gifOpen) return;  // already playing it
  closeGif();
  wantedPath = path;
  if (path.length()) {
    gifOpen = gif.open(path.c_str(), gifOpenFile, gifCloseFile, gifReadFile, gifSeekFile, GIFDraw);
  }
  if (!gifOpen) {
    if (path.length()) Serial.printf("Couldn't open %s, using the built-in GIF\n", path.c_str());
    gifOpen = gif.open((uint8_t *)gif_wave, gif_wave_len, GIFDraw);
  }
  if (!gifOpen) return;
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

// ---------------------------------------------------------------------------
// Avatar state (a port of phone/avatar.js)
// ---------------------------------------------------------------------------

static String replyEmotion;
static uint32_t replyUntil = 0;
static bool thinking = false, searching = false, talking = false, away = false;
static String activity;

String avatarState() {
  if (replyEmotion.length() && (int32_t)(replyUntil - millis()) > 0) return replyEmotion;
  if (talking) return "talking";
  if (searching) return "searching";
  if (thinking) return "thinking";
  if (away) return "sleeping";
  if (activity.length()) return activity;
  return "idle";
}

void setBusy(bool think, bool search, bool talk) {
  thinking = think;
  searching = search;
  talking = talk;
  replyEmotion = "";
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

static bool gifsSynced = false;

void onWsEvent(WStype_t type, uint8_t *payload, size_t length) {
  switch (type) {
    case WStype_CONNECTED:
      Serial.println("WebSocket connected");
      showStatus("App linked", ILI9341_GREEN);
      ws.sendTXT("{\"type\":\"hello\",\"body\":\"esp32\"}");
      gifsSynced = false;  // check for new art (done in loop, outside this callback)
      break;
    case WStype_DISCONNECTED:
      Serial.println("WebSocket disconnected");
      showStatus(cfgToken.length() ? "App offline" : "Needs pairing code", ILI9341_RED);
      break;
    case WStype_TEXT: {
      JsonDocument doc;
      if (deserializeJson(doc, payload, length)) return;
      String type = doc["type"] | "reply";
      String text = doc["text"] | "";
      if (type == "token") {
        if (!talking) setBusy(false, false, true);
        break;  // the text arrives whole in "reply"
      }
      Serial.printf("Got: %.*s\n", (int)length, (const char *)payload);
      if (type == "reply") {
        String emotion = doc["emotion"] | "neutral";
        emotion.toLowerCase();
        setBusy(false, false, false);
        replyEmotion = emotion;
        replyUntil = millis() + REPLY_HOLD_MS;
        showReply(emotion, text);
      } else if (type == "user") {
        setBusy(true, false, false);
        showReply("thinking", "\"" + text + "\"");
      } else if (type == "thinking") {
        setBusy(true, false, false);
      } else if (type == "tool") {
        setBusy(true, true, false);
        showReply("searching", doc["label"] | "looking it up");
      } else if (type == "ask") {
        showReply("may I?", text + "\n(answer on the PC or phone)");
      } else if (type == "state") {
        String s = doc["state"] | "";
        if (s == "away") away = true;
        else if (s == "active") away = false;
        else activity = s;
      }
      break;
    }
    default:
      break;
  }
}

// ---------------------------------------------------------------------------
// Wi-Fi setup and finding the app
// ---------------------------------------------------------------------------

static bool wsStarted = false;
static uint32_t nextLookupAt = 0;

void loadConfig() {
  prefs.begin("companion", true);
  cfgToken = prefs.getString("token", "");
  cfgHost = prefs.getString("host", "");
  prefs.end();
}

#ifndef WOKWI
void saveParams() {
  prefs.begin("companion", false);
  prefs.putString("token", String(tokenParam.getValue()));
  prefs.putString("host", String(hostParam.getValue()));
  prefs.end();
  loadConfig();
  Serial.println("Saved pairing code and PC address");
}

void forgetEverything() {
  showReply("reset", "Forgetting Wi-Fi and pairing code...");
  prefs.begin("companion", false);
  prefs.clear();
  prefs.end();
  wm.resetSettings();
  delay(500);
  ESP.restart();
}
#endif

// Finds the app: the saved address, or the PC advertising _companion._tcp
bool findApp() {
#ifdef WOKWI
  pcHost = FIXED_HOST;
  return true;
#else
  if (cfgHost.length()) {
    pcHost = cfgHost;
    return true;
  }
  showStatus("Finding PC...", ILI9341_YELLOW);
  int n = MDNS.queryService("companion", "tcp");
  if (n > 0) {
    pcHost = MDNS.address(0).toString();
    return true;
  }
  IPAddress ip = MDNS.queryHost("diy-waifu", 2000);
  if (ip != IPAddress((uint32_t)0)) {
    pcHost = ip.toString();
    return true;
  }
  showStatus("PC not found", ILI9341_RED);
  return false;
#endif
}

void startWs() {
  wsPath = cfgToken.length() ? "/?token=" + cfgToken : "/";
  Serial.printf("Connecting to ws://%s:%u%s\n", pcHost.c_str(), WS_PORT, cfgToken.length() ? "/?token=***" : "/");
  showStatus("Linking...", ILI9341_YELLOW);
  ws.begin(pcHost.c_str(), WS_PORT, wsPath.c_str());
  ws.onEvent(onWsEvent);
  ws.setReconnectInterval(3000);  // keeps retrying if the app isn't running yet
  wsStarted = true;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

void setup() {
  Serial.begin(115200);
  Serial.println("Booting...");
  pinMode(RESET_BUTTON, INPUT_PULLUP);

  tft.begin();
  tft.setRotation(1);
  tft.fillScreen(ILI9341_BLACK);

  gif.begin(GIF_PALETTE_RGB565_LE);
  playPath("");
  showReply("hello", "Waiting for the companion app...");
  showStatus("WiFi...", ILI9341_YELLOW);

  fsReady = LittleFS.begin(true);  // formats on first boot
  if (!fsReady) Serial.println("LittleFS unavailable; using the built-in GIF");
  loadConfig();

#ifdef WOKWI
  WiFi.begin(WIFI_SSID, "", WIFI_CHANNEL);
#else
  tokenParam.setValue(cfgToken.c_str(), 12);
  hostParam.setValue(cfgHost.c_str(), 64);
  wm.addParameter(&tokenParam);
  wm.addParameter(&hostParam);
  wm.setSaveParamsCallback(saveParams);
  wm.setConfigPortalBlocking(false);  // keep animating while the portal is up
  wm.setConnectTimeout(20);
  if (!wm.autoConnect("Companion-Setup")) {
    showReply("setup", "Join the Wi-Fi \"Companion-Setup\" on your phone to connect me to your network.");
    showStatus("Setup portal", ILI9341_YELLOW);
  }
#endif
}

void loop() {
#ifndef WOKWI
  wm.process();
  // Hold BOOT for 3s to forget Wi-Fi and the pairing code
  static uint32_t bootDownSince = 0;
  if (digitalRead(RESET_BUTTON) == LOW) {
    if (!bootDownSince) bootDownSince = millis();
    if (millis() - bootDownSince > 3000) forgetEverything();
  } else {
    bootDownSince = 0;
  }
#endif

  if (!wsStarted && WiFi.status() == WL_CONNECTED && millis() >= nextLookupAt) {
    static bool mdnsUp = false;
    if (!mdnsUp) {
      Serial.printf("WiFi connected, IP %s\n", WiFi.localIP().toString().c_str());
      mdnsUp = MDNS.begin("companion-esp32");
    }
    if (findApp()) {
      startWs();
    } else {
      nextLookupAt = millis() + 5000;
    }
  }
  if (wsStarted) {
    ws.loop();
    if (!gifsSynced && ws.isConnected()) {
      gifsSynced = true;
      syncGifs();  // closes the GIF; playPath below picks it again
    }
  }

  playPath(pathForState(avatarState()));
  tickGif();
}
