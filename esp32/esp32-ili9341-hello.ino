/*
  ESP32 + ILI9341 LCD Basic Example

  https://wokwi.com/projects/325324981270479442
*/

// #include "SPI.h"
// #include "Adafruit_GFX.h"
// #include "Adafruit_ILI9341.h"

// #define TFT_DC 2
// #define TFT_CS 15
// Adafruit_ILI9341 tft = Adafruit_ILI9341(TFT_CS, TFT_DC);

// void setup() {
//   tft.begin();

//   tft.setCursor(20, 120);
//   tft.setTextColor(ILI9341_RED);
//   tft.setTextSize(3);
//   tft.println("Hello ESP32");

//   tft.setCursor(24, 160);
//   tft.setTextColor(ILI9341_GREEN);
//   tft.setTextSize(2);
//   tft.println("I can do SPI :-)");
// }

// void loop() { delay(10); }


// #include "SPI.h"
// #include "Adafruit_GFX.h"
// #include "Adafruit_ILI9341.h"
// #include <WiFi.h>
// #include <HTTPClient.h>
// #include <AnimatedGIF.h>

// #define TFT_DC 2
// #define TFT_CS 15
// Adafruit_ILI9341 tft = Adafruit_ILI9341(TFT_CS, TFT_DC);

// const char* ssid = "Wokwi-GUEST";
// const char* password = "";
// const char* apiUrl = "https://bored-api.appbrewery.com/random";      // health-check API
// const char* gifUrl = "https://c.tenor.com/BukXoZ4HfzkAAAAC/tenor.gif";         // direct .gif URL, plain HTTP

// AnimatedGIF gif;
// uint8_t *gifBuffer = nullptr;
// size_t gifBufferSize = 0;

// // ---- AnimatedGIF draw callback: pushes one decoded line to the ILI9341 ----
// void GIFDraw(GIFDRAW *pDraw) {
//   int y = pDraw->iY + pDraw->y;
//   // pPixels already holds palette-resolved RGB565 pixels when using this mode
//   tft.startWrite();
//   tft.setAddrWindow(pDraw->iX, y, pDraw->iWidth, 1);
//   tft.writePixels((uint16_t *)pDraw->pPixels, pDraw->iWidth, false, false);
//   tft.endWrite();
// }

// // ---- Download GIF fully into a RAM buffer ----
// bool downloadGifToRam(const char *url) {
//   HTTPClient http;
//   http.begin(url);
//   int httpCode = http.GET();
//   Serial.printf("Download response code: %d\n", httpCode);

//   if (httpCode != 200) {
//     http.end();
//     return false;
//   }

//   int len = http.getSize();
//   WiFiClient *stream = http.getStreamPtr();

//   size_t cap = (len > 0) ? len : 200000;
//   gifBuffer = (uint8_t *)malloc(cap);
//   if (!gifBuffer) {
//     Serial.println("Failed to allocate RAM for GIF");
//     http.end();
//     return false;
//   }

//   size_t totalRead = 0;
//   while (http.connected() && (len > 0 || len == -1)) {
//     size_t avail = stream->available();
//     if (avail) {
//       size_t toRead = min(avail, cap - totalRead);
//       if (toRead == 0) break;
//       int c = stream->readBytes(gifBuffer + totalRead, toRead);
//       totalRead += c;
//       if (len > 0) len -= c;
//     }
//     if (len == 0) break;
//     delay(1);
//   }

//   http.end();
//   gifBufferSize = totalRead;
//   Serial.printf("Downloaded %d bytes into RAM\n", totalRead);
//   return totalRead > 0;
// }

// void playGifFromRam() {
//   if (gif.open(gifBuffer, gifBufferSize, GIFDraw)) {
//     Serial.println("GIF opened from RAM, playing...");
//     while (gif.playFrame(true, NULL)) {}
//     gif.close();
//   } else {
//     Serial.println("Failed to open GIF from RAM");
//   }
// }

// bool callApiAndCheckOk() {
//   HTTPClient http;
//   http.begin(apiUrl);
//   int httpCode = http.GET();
//   Serial.printf("API response code: %d\n", httpCode);
//   http.end();
//   return (httpCode == 200);
// }

// void setup() {
//   Serial.begin(115200);
//   delay(500);
//   Serial.println("Booting...");

//   tft.begin();
//   tft.setRotation(1);
//   tft.fillScreen(ILI9341_BLACK);
//   tft.setCursor(10, 10);
//   tft.setTextColor(ILI9341_WHITE);
//   tft.setTextSize(2);
//   tft.println("Booting...");

//   WiFi.begin(ssid, password);
//   Serial.print("Connecting to WiFi");
//   int attempts = 0;
//   while (WiFi.status() != WL_CONNECTED && attempts < 20) {
//     delay(300);
//     Serial.print(".");
//     attempts++;
//   }
//   if (WiFi.status() != WL_CONNECTED) {
//     Serial.println("\nWiFi FAILED to connect");
//     tft.println("WiFi failed");
//     return;
//   }
//   Serial.println("\nWiFi connected");

//   if (callApiAndCheckOk()) {
//     Serial.println("API OK, downloading GIF to RAM");
//     if (downloadGifToRam(gifUrl)) {
//       playGifFromRam();
//     } else {
//       tft.println("GIF download failed");
//     }
//   } else {
//     Serial.println("API not OK, skipping GIF");
//     tft.println("API not OK");
//   }

//   if (gifBuffer) {
//     free(gifBuffer);
//     gifBuffer = nullptr;
//   }
// }

// void loop() { delay(10); }



#include "SPI.h"
#include "Adafruit_GFX.h"
#include "Adafruit_ILI9341.h"
#include <WiFi.h>
#include <WiFiClientSecure.h>
#include <HTTPClient.h>
#include <AnimatedGIF.h>

#define TFT_DC 2
#define TFT_CS 15
Adafruit_ILI9341 tft = Adafruit_ILI9341(TFT_CS, TFT_DC);

const char* ssid = "Wokwi-GUEST";
const char* password = "";
const char* gifUrl = "https://www.learningcontainer.com/wp-content/uploads/2020/09/Sample-gif-Image-File-Download-for-testing.gif";

AnimatedGIF gif;
uint8_t *gifBuffer = nullptr;
size_t gifBufferSize = 0;

void GIFDraw(GIFDRAW *pDraw) {
  int y = pDraw->iY + pDraw->y;
  tft.startWrite();
  tft.setAddrWindow(pDraw->iX, y, pDraw->iWidth, 1);
  tft.writePixels((uint16_t *)pDraw->pPixels, pDraw->iWidth, false, false);
  tft.endWrite();
}

bool downloadGifToRam(const char *url) {
  WiFiClientSecure client;
  client.setInsecure();   // skip cert validation - fine for prototyping only

  HTTPClient http;
  http.begin(client, url);
  int httpCode = http.GET();
  Serial.printf("Download response code: %d\n", httpCode);

  if (httpCode != 200) {
    http.end();
    return false;
  }

  int len = http.getSize();
  WiFiClient *stream = http.getStreamPtr();

  size_t cap = (len > 0) ? len : 400000;
  gifBuffer = (uint8_t *)malloc(cap);
  if (!gifBuffer) {
    Serial.println("Failed to allocate RAM for GIF");
    http.end();
    return false;
  }

  size_t totalRead = 0;
  while (http.connected() && (len > 0 || len == -1)) {
    size_t avail = stream->available();
    if (avail) {
      size_t toRead = min(avail, cap - totalRead);
      if (toRead == 0) break;
      int c = stream->readBytes(gifBuffer + totalRead, toRead);
      totalRead += c;
      if (len > 0) len -= c;
    }
    if (len == 0) break;
    delay(1);
  }

  http.end();
  gifBufferSize = totalRead;
  Serial.printf("Downloaded %d bytes into RAM\n", totalRead);
  return totalRead > 0;
}

void playGifFromRam() {
  if (gif.open(gifBuffer, gifBufferSize, GIFDraw)) {
    Serial.println("GIF opened from RAM, playing...");
    while (gif.playFrame(true, NULL)) {}
    gif.close();
  } else {
    Serial.println("Failed to open GIF from RAM");
  }
}

void setup() {
  Serial.begin(115200);
  delay(500);
  Serial.println("Booting...");

  tft.begin();
  tft.setRotation(1);
  tft.fillScreen(ILI9341_BLACK);
  tft.setCursor(10, 10);
  tft.setTextColor(ILI9341_WHITE);
  tft.setTextSize(2);
  tft.println("Booting...");

  WiFi.begin(ssid, password);
  Serial.print("Connecting to WiFi");
  int attempts = 0;
  while (WiFi.status() != WL_CONNECTED && attempts < 20) {
    delay(300);
    Serial.print(".");
    attempts++;
  }
  if (WiFi.status() != WL_CONNECTED) {
    Serial.println("\nWiFi FAILED to connect");
    tft.println("WiFi failed");
    return;
  }
  Serial.println("\nWiFi connected");

  Serial.println("Downloading GIF to RAM");
  if (downloadGifToRam(gifUrl)) {
    playGifFromRam();
  } else {
    tft.println("GIF download failed");
  }

  if (gifBuffer) {
    free(gifBuffer);
    gifBuffer = nullptr;
  }
}

void loop() {}