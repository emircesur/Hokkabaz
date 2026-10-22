#!/usr/bin/env bash
# Creates a release keystore + apps/flutter/android/key.properties for signing the APK.
# Keep the .jks and its passwords safe: losing them means you can never update the app in place.
set -euo pipefail
cd "$(dirname "$0")/../apps/flutter/android"
KS="${1:-hokkabaz-release.jks}"
ALIAS="${KEY_ALIAS:-hokkabaz}"
PASS="${KEYSTORE_PASSWORD:-$(openssl rand -base64 18 | tr -d '/+=')}"
[ -e "$KS" ] && { echo "$KS already exists; refusing to overwrite" >&2; exit 1; }
keytool -genkeypair -v -keystore "$KS" -storetype PKCS12 -alias "$ALIAS" -keyalg RSA -keysize 4096 \
  -validity 10000 -storepass "$PASS" -keypass "$PASS" -dname "CN=Hokkabaz, O=Hokkabaz"
cat > key.properties <<PROPS
storeFile=$KS
storePassword=$PASS
keyAlias=$ALIAS
keyPassword=$PASS
PROPS
echo "Wrote android/$KS and android/key.properties (both are git-ignored)."
echo "For CI: base64 -w0 $KS  -> secret KEYSTORE_B64; secrets KEYSTORE_PASSWORD=$PASS, KEY_ALIAS=$ALIAS"
