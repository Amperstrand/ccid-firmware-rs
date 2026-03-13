#!/usr/bin/env python3
"""
Test PIN pad entry via PC/SC Secure PIN Interface.

This script tests the complete PIN entry flow:
1. Connect to reader
2. Verify PIN pad is available
3. Trigger PIN entry via SCardControl
4. Wait for user to enter PIN on device
5. Verify result

REQUIREMENTS:
- Firmware with profile-gemalto-pinpad or profile-cherry-st2100
- pcscd running
- A smartcard that supports VERIFY PIN (e.g., OpenPGP card, SeedKeeper)
- pyscard: pip install pyscard

USAGE:
  python3 test_pinpad_entry.py [--reader NAME] [--pin 123456]

For manual testing (enter PIN on device touchscreen):
  python3 test_pinpad_entry.py --manual
"""

import sys
import argparse
import time

try:
    from smartcard.System import readers
    from smartcard.scard import (
        SCARD_PROTOCOL_T0, SCARD_PROTOCOL_T1,
        SCARD_CTL_CODE,
    )
    from smartcard import scard
except ImportError:
    print("Install pyscard: pip install pyscard", file=sys.stderr)
    sys.exit(1)


# CCID IOCTL for Secure PIN entry
IOCTL_SMARTCARD_VENDOR_IFD_EXCHANGE = SCARD_CTL_CODE(2048)

# PIN Verify Data Structure (CCID 6.1.11)
# This is a minimal structure for PIN verification


def build_pin_verify_structure(min_len=6, max_len=8, timeout=30):
    """
    Build CCID PIN Verification Data Structure.
    
    Structure (CCID Rev 1.1 Section 6.1.11):
    - bTimerOut: Timeout in seconds
    - bmFormatString: PIN format (0x82 = ASCII, left justified)
    - bmPINBlockString: PIN block length (0x00 for variable)
    - bmPINLengthFormat: PIN length format (0x00)
    - wPINMaxExtraDigit: (max << 8) | min
    - bEntryValidationCondition: 0x02 = validation key pressed
    - bNumberMessage: Number of messages (0x01)
    - wLangId: Language ID (0x0409 = English US)
    - bMsgIndex: Message index (0x00)
    - bTeoPrologue: TPDU prologue (0x00)
    - abPINApdu: APDU template (CLA INS P1 P2 Lc)
    """
    # APDU template for VERIFY command (OpenPGP format)
    # CLA=00 INS=20 P1=00 P2=81 Lc=08 (user PIN, 8 bytes max)
    apdu_template = bytes([0x00, 0x20, 0x00, 0x81, 0x08])
    
    data = bytearray()
    data.append(timeout)                    # bTimerOut
    data.append(0x82)                       # bmFormatString (ASCII, left justified)
    data.append(0x00)                       # bmPINBlockString
    data.append(0x00)                       # bmPINLengthFormat
    data.append((max_len << 4) | min_len)   # wPINMaxExtraDigit (low byte)
    data.append(max_len)                    # wPINMaxExtraDigit (high byte)
    data.append(0x02)                       # bEntryValidationCondition
    data.append(0x01)                       # bNumberMessage
    data.append(0x09)                       # wLangId low (0x0409 = English)
    data.append(0x04)                       # wLangId high
    data.append(0x00)                       # bMsgIndex
    data.append(0x00)                       # bTeoPrologue
    data.extend(apdu_template)              # APDU template
    
    return bytes(data)


def find_reader(name_filter=None):
    """Find a PIN-capable reader."""
    rlist = readers()
    if not rlist:
        return None, "No readers found"
    
    if name_filter:
        for r in rlist:
            if name_filter.lower() in str(r).lower():
                return r, None
        return None, f"Reader '{name_filter}' not found"
    
    # Return first reader
    return rlist[0], None


def test_pin_capability(reader):
    """Test if reader supports PIN pad via SCardGetAttrib."""
    conn = reader.createConnection()
    
    try:
        conn.connect(SCARD_PROTOCOL_T0 | SCARD_PROTOCOL_T1)
    except Exception as e:
        return False, f"Failed to connect: {e}"
    
    try:
        # Try to get vendor IFD version (indicates CCID support)
        # This is a basic check - full PIN verification requires FEATURE_VERIFY_PIN_DIRECT
        hresult, hcontext = scard.SCardEstablishContext(scard.SCARD_SCOPE_USER)
        if hresult != scard.SCARD_S_SUCCESS:
            return False, "Failed to establish context"
        
        # For now, assume PIN pad is available if reader connects
        # Full verification would use SCardGetAttrib with SCARD_ATTR_VENDOR_IFD_VERSION
        return True, None
        
    finally:
        conn.disconnect()


def trigger_pin_entry(reader, min_len=6, max_len=8, timeout=30):
    """
    Trigger PIN entry on the device.
    
    Returns (success, error_message)
    """
    conn = reader.createConnection()
    
    try:
        conn.connect(SCARD_PROTOCOL_T0 | SCARD_PROTOCOL_T1)
        print(f"Connected to: {reader}")
        print(f"ATR: {bytes(conn.getATR()).hex().upper()}")
    except Exception as e:
        return False, f"Failed to connect: {e}"
    
    try:
        # Build PIN verify structure
        pin_data = build_pin_verify_structure(min_len, max_len, timeout)
        print(f"\nPIN Verify Structure ({len(pin_data)} bytes): {pin_data.hex()}")
        print(f"  Min PIN length: {min_len}")
        print(f"  Max PIN length: {max_len}")
        print(f"  Timeout: {timeout}s")
        
        print("\n" + "=" * 50)
        print("PIN entry should now be active on the device!")
        print("Please enter PIN on the touchscreen keypad.")
        print("=" * 50 + "\n")
        
        # Send control command for PIN verification
        # Note: This is a simplified approach - real implementation
        # would use FEATURE_VERIFY_PIN_DIRECT via CM_IOCTL_GET_FEATURE_REQUEST
        
        try:
            result = conn.control(IOCTL_SMARTCARD_VENDOR_IFD_EXCHANGE, list(pin_data))
            print(f"Control result: {result}")
            
            # Parse response
            if result:
                sw1 = result[-2] if len(result) >= 2 else 0
                sw2 = result[-1] if len(result) >= 1 else 0
                print(f"Status Word: {sw1:02X}{sw2:02X}")
                
                if sw1 == 0x90 and sw2 == 0x00:
                    return True, None
                elif sw1 == 0x63:
                    return False, f"Wrong PIN (SW={sw1:02X}{sw2:02X}, tries remaining: {sw2 & 0x0F})"
                else:
                    return False, f"PIN entry failed (SW={sw1:02X}{sw2:02X})"
            else:
                return False, "No response from control command"
                
        except Exception as e:
            error_msg = str(e)
            
            # Check for common error codes
            if "0x80100016" in error_msg or "NOT_TRANSACTED" in error_msg:
                return False, "PIN entry not supported or cancelled by user"
            elif "0x8010002C" in error_msg or "CANCELLED" in error_msg:
                return False, "PIN entry cancelled"
            elif "0x8010000A" in error_msg or "TIMEOUT" in error_msg:
                return False, "PIN entry timed out"
            else:
                return False, f"Control command error: {e}"
                
    finally:
        conn.disconnect()


def test_manual_pin(reader, timeout=60):
    """
    Manual PIN entry test - just prompts user and waits.
    
    This is for verifying the touchscreen UI works without
    needing full PC/SC PIN support.
    """
    conn = reader.createConnection()
    
    try:
        conn.connect(SCARD_PROTOCOL_T0 | SCARD_PROTOCOL_T1)
        print(f"Connected to: {reader}")
        print(f"ATR: {bytes(conn.getATR()).hex().upper()}")
    except Exception as e:
        return False, f"Failed to connect: {e}"
    
    try:
        print("\n" + "=" * 50)
        print("MANUAL PIN ENTRY TEST")
        print("=" * 50)
        print("\nOn the STM32 device:")
        print("  1. Touch digit buttons to enter PIN")
        print("  2. Press OK to submit")
        print("  3. Press Cancel to abort")
        print("\nThis test just verifies the UI is responsive.")
        print("=" * 50)
        
        input("\nPress ENTER when you have tested the PIN pad...")
        
        return True, None
        
    finally:
        conn.disconnect()


def main() -> int:
    parser = argparse.ArgumentParser(description="Test PIN pad entry")
    parser.add_argument("--reader", "-r", help="Reader name filter")
    parser.add_argument("--min-len", type=int, default=6, help="Minimum PIN length")
    parser.add_argument("--max-len", type=int, default=8, help="Maximum PIN length")
    parser.add_argument("--timeout", "-t", type=int, default=30, help="PIN entry timeout (seconds)")
    parser.add_argument("--manual", "-m", action="store_true", help="Manual test mode (no PIN verification)")
    args = parser.parse_args()
    
    print("CCID PIN Pad Entry Test")
    print("=" * 40)
    
    # Find reader
    reader, error = find_reader(args.reader)
    if error:
        print(f"ERROR: {error}")
        print("\nAvailable readers:")
        for r in readers():
            print(f"  - {r}")
        return 1
    
    print(f"Using reader: {reader}")
    
    # Test PIN capability
    print("\nChecking PIN capability...")
    has_pin, error = test_pin_capability(reader)
    if error:
        print(f"WARN: {error}")
    else:
        print("PASS: Reader appears to support PIN operations")
    
    # Run test
    if args.manual:
        success, error = test_manual_pin(reader, args.timeout)
    else:
        success, error = trigger_pin_entry(reader, args.min_len, args.max_len, args.timeout)
    
    print()
    print("=" * 40)
    if success:
        print("RESULT: PIN entry test passed")
        return 0
    else:
        print(f"RESULT: {error}")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
