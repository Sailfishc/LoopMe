# Internal TestFlight releases

The iOS app uses team `35KYWWD972` and bundle ID
`com.sailfishc.loopme.ios`, matching the paired production device installation
and the Mac daemon's APNs topic. Keep this identifier when releasing updates.

## Archive and upload

Run from the repository root with Xcode signed into the developer account.
Set `MARKETING_VERSION` and increment `CURRENT_PROJECT_VERSION` in
`apple/project.yml` before each new upload. The generated Info.plist uses these
values directly.

```sh
(cd apple && xcodegen generate)
mkdir -p apple/build/TestFlight
xcodebuild -project apple/LoopMe.xcodeproj -scheme LoopMe \
  -configuration Release -destination 'generic/platform=iOS' \
  -archivePath apple/build/TestFlight/LoopMe.xcarchive \
  -allowProvisioningUpdates archive
xcodebuild -exportArchive \
  -archivePath apple/build/TestFlight/LoopMe.xcarchive \
  -exportOptionsPlist apple/ExportOptions-TestFlight.plist \
  -exportPath apple/build/TestFlight/export \
  -allowProvisioningUpdates
```

The export uploads directly to App Store Connect and restricts the build to
internal TestFlight testing. Wait for Apple processing, then verify the build
is available to the intended internal group. Upload success alone does not
establish tester availability or device acceptance.

Release builds register with the production APNs environment; confirm the
exported provisioning profile grants `aps-environment=production` and
`get-task-allow=false`. The Mac daemon must have APNs configured for the same
bundle ID. No APNs private key belongs in the repository.

The iOS app uses Apple's CryptoKit and Security frameworks with no bundled
third-party crypto library. `ITSAppUsesNonExemptEncryption=false` records this
use of exempt system encryption; reassess if the implementation changes.
See Apple's [export compliance documentation](https://developer.apple.com/help/app-store-connect/reference/app-information/export-compliance-documentation-for-encryption/).
