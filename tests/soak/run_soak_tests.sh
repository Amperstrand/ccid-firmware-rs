#!/bin/bash
set -e
RIG_DIR="/home/ubuntu/gt/ccid_firmware/mayor/rig"
LOG_DIR="/home/ubuntu/gt/ccid_firmware/soak-test-logs"
REPORT="$LOG_DIR/run.log"

mkdir -p "$LOG_DIR"

echo "========================================" | tee -a "$REPORT"
echo "CCID FIRMWARE SOAK TEST RUN" | tee -a "$REPORT"
echo "Started: $(date -u '+%Y-%m-%d %H:%M:%S UTC')" | tee -a "$REPORT"
echo "Commit: $(cd $RIG_DIR && git rev-parse --short HEAD)" | tee -a "$REPORT"
echo "========================================" | tee -a "$REPORT"

SCRIPTS=(
    "soak_02_globalplatform.py:soak-02-globalplatform:GlobalPlatform"
    "soak_03_satochip.py:soak-03-satochip:SatoChip"
    "soak_04_openpgp.py:soak-04-openpgp:OpenPGP"
    "soak_05_piv.py:soak-05-piv:PIV"
    "soak_06_opensc.py:soak-06-opensc-toolchain:OpenSC"
    "soak_07_gpg.py:soak-07-gpg-card-edit:GPG"
    "soak_08_extended_apdu.py:soak-08-extended-apdu:ExtendedAPDU"
    "soak_09_stress.py:soak-09-stress-repeat:Stress"
    "soak_10_cross_compare.py:soak-10-cross-compare:CrossCompare"
)

TOTAL=${#SCRIPTS[@]}
PASSED=0
FAILED=0
FAILED_LIST=""

for entry in "${SCRIPTS[@]}"; do
    IFS=':' read -r script name label <<< "$entry"
    echo "" | tee -a "$REPORT"
    echo "[$(date -u '+%H:%M:%S')] Running $name ($label)..." | tee -a "$REPORT"
    
    START=$(date +%s)
    if sudo timeout 600 python3 "$RIG_DIR/tests/soak/$script" >> "$LOG_DIR/${name}.stdout" 2>> "$LOG_DIR/${name}.stderr"; then
        RC=0
    else
        RC=$?
    fi
    END=$(date +%s)
    ELAPSED=$((END - START))
    
    if [ $RC -eq 0 ]; then
        echo "  [$name] PASS (${ELAPSED}s)" | tee -a "$REPORT"
        PASSED=$((PASSED + 1))
    else
        echo "  [$name] FAIL (rc=$RC, ${ELAPSED}s)" | tee -a "$REPORT"
        FAILED=$((FAILED + 1))
        FAILED_LIST="$FAILED_LIST $name"
    fi
    
    echo "  Sleeping 5s before next suite..." | tee -a "$REPORT"
    sleep 5
done

echo "" | tee -a "$REPORT"
echo "========================================" | tee -a "$REPORT"
echo "FINAL RESULTS" | tee -a "$REPORT"
echo "  Passed: $PASSED/$TOTAL" | tee -a "$REPORT"
echo "  Failed: $FAILED" | tee -a "$REPORT"
if [ -n "$FAILED_LIST" ]; then
    echo "  Failed suites:$FAILED_LIST" | tee -a "$REPORT"
fi
echo "  Completed: $(date -u '+%Y-%m-%d %H:%M:%S UTC')" | tee -a "$REPORT"
echo "  Logs: $LOG_DIR" | tee -a "$REPORT"
echo "========================================" | tee -a "$REPORT"
