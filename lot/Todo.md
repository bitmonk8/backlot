# Unity Sandbox — Less Critical Denials

Remaining sandbox denials that do not block licensing or core startup.
These affect UI features, preferences persistence, and optional system integration.

## SBPL rules needed in seatbelt.rs

### IOKit (GPU / HID)
- `iokit-open-user-client AGXDeviceUserClient` — Apple GPU access
- `iokit-open-user-client IOSurfaceRootUserClient` — IOSurface (rendering)
- `iokit-open-user-client IOHIDParamUserClient` — HID parameter access (mouse/keyboard)

Consider: `(allow iokit-open-user-client)` unrestricted, or enumerate specific classes.

### Mach port registration
- `mach-register com.apple.axserver` — accessibility server
- `mach-register com.apple.tsm.portname` — text services manager (input methods)
- `mach-register com.apple.coredrag` — drag and drop

These are per-pid registrations. Needs `(allow mach-register)` or scoped to specific names.

### User preferences
- `user-preference-read com.apple.hitoolbox` — HI Toolbox (UI framework)
- `user-preference-write com.unity3d.unityeditor5.x` — Unity editor preferences

Needs `(allow user-preference-read)` and `(allow user-preference-write)` rules.
Could be scoped to specific domain names or left unrestricted.

### Filesystem control
- `system-fsctl (_IO "h" 47)` — filesystem ioctl, likely HFS/APFS related

Needs `(allow system-fsctl)` or scoped to specific fsctl codes.

### Signal to children
- `signal children [UnityPackageManager] signum:15` — Unity can't SIGTERM child processes

Current profile only allows `(allow signal (target self))`.
Fix: change to `(allow signal)` (unrestricted) or find a way to scope to children.
Note: lot's Drop impl uses `killpg` from the parent (unsandboxed) process, so children
are still cleaned up — this only affects graceful shutdown initiated by the sandboxed process.

## Paths to add to unity-scanner.yaml

### Read paths
- `/Library/Caches/com.apple.iconservices.store` — icon service cache (UI)
- `/private/var/db/.AppleSetupDone` — system setup detection
- `/Library/Audio/Plug-Ins/HAL` — audio HAL plugins (e.g. ZoomAudioDevice.driver)

### Misc
- `file-issue-extension` for the Unity app bundle — app sandbox extension issuance; may not be fixable via SBPL
- `file-read-data /dev/dtracehelper` — dtrace helper device; harmless denial, no fix needed
- `file-read-data /dev/autofs_nowait` — automounter; harmless denial, no fix needed
