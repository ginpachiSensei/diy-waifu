# PlatformIO post-build step: writes .pio/build/<env>/merged-firmware.bin, a full 4 MB
# flash image (bootloader + partition table + app). Wokwi loads this so it uses our
# huge_app partition layout instead of its default one, which is too small for the app.
Import("env")

APP_BIN = "$BUILD_DIR/${PROGNAME}.bin"
MERGED_BIN = "$BUILD_DIR/merged-firmware.bin"
board = env.BoardConfig()


def merge_bin(source, target, env):
    images = env.Flatten(env.get("FLASH_EXTRA_IMAGES", [])) + ["$ESP32_APP_OFFSET", APP_BIN]
    env.Execute(" ".join([
        '"$OBJCOPY"',  # esptool executable shipped with the espressif32 platform
        "--chip", board.get("build.mcu", "esp32"),
        "merge-bin", "--pad-to-size", board.get("upload.flash_size", "4MB"),
        "-o", MERGED_BIN,
    ] + [f'"{i}"' for i in images]))


env.AddPostAction(APP_BIN, merge_bin)
