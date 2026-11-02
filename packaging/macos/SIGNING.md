# Signing and notarising the Mac app

The release builds sign `Fokus.app` and the DMG with a *Developer ID Application* certificate and
have Apple notarise both, so macOS opens Fokus without a warning. This needs five repository
secrets; while they are missing, the Mac builds are packaged unsigned.

| Secret | What it is |
|---|---|
| `MACOS_CERTIFICATE_P12` | The Developer ID Application certificate with its private key, exported as .p12, base64 |
| `MACOS_CERTIFICATE_PASSWORD` | The password chosen when exporting the .p12 |
| `NOTARY_KEY_P8` | An App Store Connect API key (.p8 file) with the Developer role |
| `NOTARY_KEY_ID` | That key's Key ID |
| `NOTARY_ISSUER` | The Issuer ID shown above the list of keys |

## Getting them

Needs a paid Apple Developer Program membership; only the team's Account Holder can create a
Developer ID certificate.

1. **Certificate.** Xcode → Settings → Accounts → the team → *Manage Certificates…* → **+** →
   *Developer ID Application*. (Or developer.apple.com → Certificates, IDs & Profiles →
   Certificates → **+**, with a signing request from Keychain Access → Certificate Assistant.)
2. **Export it.** Keychain Access → *My Certificates* → right-click *Developer ID Application: …*
   → *Export…* → .p12 with a password.
3. **Notary key.** App Store Connect → Users and Access → Integrations → App Store Connect API →
   Team Keys → generate a key with the **Developer** role and download the .p8 (possible only
   once).

## Storing them

```bash
base64 -i Certificates.p12 | gh secret set MACOS_CERTIFICATE_P12 -R mbirnbach/fokus
gh secret set MACOS_CERTIFICATE_PASSWORD -R mbirnbach/fokus    # asks for the value
gh secret set NOTARY_KEY_P8 -R mbirnbach/fokus < AuthKey_XXXXXXXXXX.p8
gh secret set NOTARY_KEY_ID -R mbirnbach/fokus
gh secret set NOTARY_ISSUER -R mbirnbach/fokus
```

The certificate is valid for five years; renew it the same way and replace the first two
secrets. Apps signed with the old one keep working.

## Locally

`scripts/package.sh` signs when `MACOS_SIGN_IDENTITY` names a certificate in the keychain, and
notarises when it also has `NOTARY_KEY` (path to the .p8), `NOTARY_KEY_ID` and `NOTARY_ISSUER`,
or `NOTARY_PROFILE` (a profile saved with `xcrun notarytool store-credentials`).

```bash
MACOS_SIGN_IDENTITY="Developer ID Application: …" NOTARY_PROFILE=fokus scripts/package.sh aarch64-apple-darwin
spctl -a -vv -t open --context context:primary-signature dist/fokus-*-aarch64-apple-darwin.dmg
```
