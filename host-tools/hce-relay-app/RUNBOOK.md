# RUNBOOK — next phone window (bench HCE relay, #529)

State at park (window D 2026-10-31 ~22:00): app + host brain built and
bench-proven to the phone↔host link; the AID pivot (73ae7c7ac) registers
the dispatch-proven boltcard AID `D2760000850101` in the relay's PAYMENT
group. Everything below is one pass from a cold bench.

## Preconditions (check before starting)

1. S20 on adb: `adb devices` shows `RF8NB01ZYAH`. (Off-bus since ~21:20
   2026-10-31 — owner hands-window prep; see SEATS.md.)
2. NFC on, phone coupled to the nucula coil (morning proof: HCE HAL
   cycles matching the reader poll cadence).
3. pcscd carries `Nucula VPCD wifi67` (wifi-vpcd path — no USB needed).
4. APK with BOTH AID filters: `aapt2 dump xmltree bench-relay.apk
   --file res/xml/apdus.xml` must show `A000000151000000` AND
   `D2760000850101` (built OK at park time; rebuild with `./build.sh`).

## Bring-up (per window)

```bash
S=RF8NB01ZYAH
adb -s $S install -r host-tools/hce-relay-app/build/bench-relay.apk
# Boltcard HCE apps OFF (prefix-AID + default ambiguity — #529 comment):
adb -s $S shell pm disable-user dev.boltcard.hce
adb -s $S shell pm disable-user com.amperstrand.boltcard.hce
# Relay takes the payment default:
adb -s $S shell settings put secure nfc_payment_default_component \
    com.amperstrand.benchrelay/.RelayApduService
# Battery-optimization exemption (Samsung app freezer — #529):
adb -s $S shell dumpsys deviceidle whitelist +com.amperstrand.benchrelay
# Socket owner up (NFC binds HCE lazily; the kickstart owns the socket):
adb -s $S shell am start-foreground-service \
    -n com.amperstrand.benchrelay/.RelayKickstartService
# Host brain (terminal 1) — answers SELECT with 9000, echoes on others:
python3 host-tools/hce_relay.py --script - <<'EOF'
00A4040007D276000085010100=9000
EOF
```

## Probe (through pcscd, the real reader path)

`SCardConnect(Nucula VPCD wifi67)` → SCardTransmit `00 A4 04 00 07 D2 76
00 00 85 01 01 00`. The PASS condition is the C-APDU appearing in the
hce_relay log with your scripted answer returned — NOT merely a SW from
the phone (system-level 6999 interception is exactly what a
no-dispatch looks like; the #529 correction: 6999 is phone-originated,
the firmware error path is 6400/absent).

### Outcome table

| Observation | Meaning | Next |
|---|---|---|
| C-APDU in relay log, scripted SW back | payment-group dispatch to the relay WORKS | run the `--delay-ms` instruments (#472 legs); grow the script table |
| 6999, no relay log line | Samsung intercepts this AID in payment too | try PPSE (`2PAY.SYS.DD01`) + `A000000151000000` route; compare `dumpsys nfc` service routing |
| pcscd sees stale-session ATR echo (`resp+SW` = ATR bytes) | routing change burned the ISO-DEP session | field-cycle: `adb shell svc nfc disable && sleep 10 && svc nfc enable`, reconnect |
| no reader at all | phone moved / NFC off | verify `svc nfc status`, coil coupling |

## Teardown (restore the phone baseline)

```bash
adb -s $S shell pm enable dev.boltcard.hce
adb -s $S shell pm enable com.amperstrand.boltcard.hce
adb -s $S shell settings put secure nfc_payment_default_component \
    com.google.android.gms/.tapandpay.hce.service.TapAndPayHceService  # (verify current default first: settings get secure nfc_payment_default_component)
adb -s $S shell am stop-service com.amperstrand.benchrelay/.RelayKickstartService
adb -s $S forward --remove tcp:4871
```

Record the window in `/root/bench/SEATS.md` and the evidence under
`.omo/evidence/529-relay/<date>/`.
