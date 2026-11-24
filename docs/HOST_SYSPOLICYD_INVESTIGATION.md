# Host investigation: syspolicyd spin and dyld stalls

- Date: 2026-11-24
- Host: VerlindeMacBook-Pro, macOS 27.0.1 (26A434), Apple Silicon, SIP enabled
- Status: Host issue; not a VerlinPark code defect. Mitigation is host-level and
  not applied automatically.
- Related: [V1 readiness ledger](V1_READINESS_LEDGER.md)

## Symptom

During or immediately after large Rust builds/tests, `syspolicyd`
(`/usr/libexec/syspolicyd`) repeatedly entered a sustained ~530–550% CPU
state. While it spun, newly built Rust binaries — including test binaries under
`target/debug/deps` and a minimal `/tmp` Rust program — hung in `_dyld_start`.
Already-cached/Apple-signed binaries such as `/bin/echo` still ran. Killing
stuck test binaries did not clear it. A reboot did.

`sudo launchctl kickstart -k system/com.apple.security.syspolicy` failed with
`150: Operation not permitted while System Integrity Protection is engaged`.

## Root cause (high confidence)

A macOS `syspolicyd` + XProtect malware-scan defect, not a VerlinPark bug:

- On 2026-11-23, `syspolicyd` emitted 39,482 log lines in 90 minutes, including
  `failed to call driver: 0x3` in 15 batches of ~1,000 calls at exactly
  6m51s intervals. On 2026-11-24, the same pattern appeared at 00:28–00:43,
  with ~2,864 `Couldn't find a cached target … during malware scan` and
  errSecCSUnsigned (`-67062`) lines.
- The `XprotectService` connection was activated/invalidated heavily, then had
  no connections while `failed to call driver: 0x3` continued. The daemon kept
  retrying a dead scan connection instead of backing off or reconnecting.
- Every binary with a **new cdhash** needs a fresh assessment; a rebuilt test
  binary has a new path/cdhash and cannot reuse the previous scan. The exec
  policy gate waits for the assessment, so new binaries hang early in dyld.
- A captured 2026-11-22 CPU report shows the same daemon wedged in code-signing
  and bundle enumeration (`SecStaticCodeCreateWithPath`, `_CFBundleCreate…`,
  `readdir`), with thousands of trust/signature calls. This is a second
  stress path, but the same "policy daemon saturates" failure mode.

Known reports with the same error/behavior include Apple Discussions threads
255504566 and 255944686, and `openai/codex` issues 16767 and 17225 (macOS 27:
syspolicyd/trustd spin and app-launch stalls when many unique executables are
spawned; `launchctl kickstart` is SIP-blocked).

## Why this repository amplifies the trigger

This is an amplifier, not the root cause:

- `target/debug/deps` grew to **322,866 entries / 134 GB**, including 201
  executables totaling 7 GB.
- Representative test binaries are ~249 MB, with `__TEXT` 126 MB,
  `__eh_frame` 19 MB (above ld's 16 MB warning), `__LINKEDIT` 131 MB, and a
  ~2 MB ad-hoc CodeDirectory / 63,236 page hashes.
- A full rebuild touches thousands of files and relinks many test binaries in a
  short window; the 2026-11-24 storm began immediately after 9,270 deps files
  were touched.
- `DevToolsSecurity -status` reports disabled, and TCC has Developer Tools
  denied for `com.apple.Terminal`, so no process tree has the Developer Tools
  exemption. Every new binary gets a full assessment.

## Mitigations

### Host (highest leverage; requires admin)

1. Give the terminal/Orca the macOS Developer Tools exemption:
   - `sudo spctl developer-mode enable-terminal`
   - System Settings → Privacy & Security → Developer Tools → enable/add
     **Orca** (and Terminal if used); restart the app.
   - Verify with a fresh test binary; a warm `--list` should be fast.
   - Trade-off: this disables XProtect/Gatekeeper checks for children of that
     app. Do not enable it for browsers or untrusted shells.
2. Recovery if the daemon wedges:
   - stop the build/test spawner first;
   - `sudo killall syspolicyd` (launchd respawns it), or reboot.
   - Do not expect `launchctl kickstart` to work under SIP.
3. Optional: exclude `target/` from Spotlight indexing to reduce unrelated
   filesystem churn. This is a side benefit, not the fix.

### Repository/build (reduce trigger probability, not a cure)

- Consider `[profile.test] debug = 1` (or `0`) in a measured experiment. The
  current 249 MB binaries with 2 MB CodeDirectories are expensive to assess.
- Prune `target/` after large rounds (`just clean` is already documented for
  worktrees); 134 GB / 322k entries is extreme.
- Keep per-worktree target directories.
- Prefer one full-suite run over repeated kill/retry churn. Killing a binary
  does not clear the daemon retry loop and adds exec churn.
- Do not remove ad-hoc signing (`-no_adhoc_codesign`): unsigned binaries have no
  cdhash and can never be cached, making the assessment load worse.

### Monitoring

```bash
log show --last 2m --style compact --predicate \
  'process == "syspolicyd" AND eventMessage CONTAINS "failed to call driver"'
```

Any output means the wedge is starting. Capture `sample <hung-pid> 2` and a
`sysdiagnose` before killing; then stop the spawner and use the recovery above.

## Do not

- Do not disable SIP (`csrutil disable`), `spctl --master-disable`, or turn
  Gatekeeper off.
- Do not kill `amfid`, `trustd`, `xprotectd`, or `XprotectService`.
- Do not delete `/var/db/SystemPolicyConfiguration` or chase
  `DetachedSignatures`.
- Do not treat repeated reboots as the fix; rebooting clears the current wedge
  but re-arms the same bug.

## Caveats

- The 530–550% figure is from observation and log-rate inference; the captured
  2026-11-22 CPU report shows a different 97%-on-two-threads sub-path.
- The exact assessed object is redacted (`<private>`), so the "new test
  binaries are the trigger" link rests on timing correlation plus the known
  exec-assessment mechanism, not a direct path in the log.
- The current boot is healthy. The final full workspace passed on this boot:
  4,570 passed, 0 failed, 6 ignored; clippy exit 0. This does not mean the host
  issue is fixed; it is intermittent and re-arms after the trigger conditions
  recur.

## References

- Apple Discussions: "SysPolicyD Running on High CPU for hours" (thread
  255504566), "failed to call driver: 0x3" (thread 255944686).
- `openai/codex` issues 16767 and 17225 (macOS 27 syspolicyd/trustd spin).
- Apple diagnostic:
  `/Library/Logs/DiagnosticReports/syspolicyd_2026-11-22-062002_VerlindeMacBook-Pro.cpu_resource.diag`.
