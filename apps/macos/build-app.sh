#!/bin/sh
# Assemble Totsuka.app without Xcode, for trying the app on this Mac.
#
#   apps/macos/build-app.sh          # → apps/macos/build/Totsuka.app
#   open apps/macos/build/Totsuka.app
#   open --env XDG_CONFIG_HOME=… --env XDG_DATA_HOME=… … apps/macos/build/Totsuka.app
#                                    # against an isolated environment
#
# The bundle ID is `io.github.tomoya-k31.totsuka.dev` ("Totsuka Dev"), so its
# settings, notification permission and Keychain entry stay apart from the
# installed app's.
#
# The shipped app is built by CI from project.yml (macos-app.yml); that needs
# Xcode's `actool` for the asset catalog. This builds the same code with
# SwiftPM and stands in for the catalog with what Command Line Tools has:
# `iconutil` turns the AppIcon PNGs into an .icns, and the menu bar image is
# copied as plain PNGs (a name ending in "Template" makes it a template image).
# Signed ad-hoc, as the release is. Not under /tmp: macOS refuses
# notifications to an app there.
set -eu
cd "$(dirname "$0")"

swift build -c release --product TotsukaApp
bin="$(swift build -c release --show-bin-path)/TotsukaApp"
version="$(awk '/MARKETING_VERSION:/ { print $2; exit }' project.yml)"

app=build/Totsuka.app
rm -rf "${app}"
mkdir -p "${app}/Contents/MacOS" "${app}/Contents/Resources"
cp "${bin}" "${app}/Contents/MacOS/Totsuka"

iconset="$(mktemp -d)/AppIcon.iconset"
mkdir -p "${iconset}"
cp Resources/Assets.xcassets/AppIcon.appiconset/*.png "${iconset}/"
iconutil -c icns -o "${app}/Contents/Resources/AppIcon.icns" "${iconset}"
cp Resources/Assets.xcassets/StatusBar*.imageset/*.png "${app}/Contents/Resources/"

cat > "${app}/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>io.github.tomoya-k31.totsuka.dev</string>
<key>CFBundleName</key><string>Totsuka Dev</string>
<key>CFBundleDisplayName</key><string>Totsuka Dev</string>
<key>CFBundleExecutable</key><string>Totsuka</string>
<key>CFBundleIconFile</key><string>AppIcon</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>${version}</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSMinimumSystemVersion</key><string>15.0</string>
<key>LSUIElement</key><true/>
</dict></plist>
PLIST

codesign --force --sign - "${app}"
echo "built ${PWD}/${app} (${version})"
