#!/bin/sh
# `swift test` for TotsukaKit — also on Command Line Tools alone, whose
# swift-testing needs its framework path spelled out and whose
# _Testing_Foundation overlay ships without a module (so cross-import overlays
# are turned off). With Xcode selected, plain `swift test` is all it takes.
set -eu
cd "$(dirname "$0")"
case "$(xcode-select -p)" in
*CommandLineTools*)
  F=/Library/Developer/CommandLineTools/Library/Developer/Frameworks
  exec swift test -Xswiftc -F"$F" -Xswiftc -Xfrontend -Xswiftc -disable-cross-import-overlays \
    -Xlinker -F"$F" -Xlinker -rpath -Xlinker "$F" "$@"
  ;;
*) exec swift test "$@" ;;
esac
