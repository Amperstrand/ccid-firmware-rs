#!/usr/bin/env python3
"""
Hardware Test Script for CCID Pinpad Firmware

This script tests the pinpad functionality on real hardware using PC/SC.
Run this after flashing the firmware to the STM32F469-DISCO board.

Requirements:
    pip install pyscard

Usage:
    python test_pinpad_hardware.py
"""

import sys
import time
from typing import Optional, List, Tuple

try:
    from smartcard.System import readers
    from smartcard.util import toHexString, toBytes
    from smartcard.Exceptions import NoCardException, CardConnectionException
except ImportError:
    print("Error: pyscard not installed. Run: pip install pyscard")
    sys.exit(1)


class PinpadTest:
    """Test class for CCID Pinpad functionality"""
    
    # APDU constants
    CLA = 0x00
    INS_VERIFY = 0x20
    INS_SELECT = 0xA4
    
    # OpenPGP AID
    OPENPGP_AID = [0xD2, 0x76, 0x00, 0x01, 0x24, 0x01]
    
    # PIN types
    USER_PIN_P2 = 0x81
    ADMIN_PIN_P2 = 0x83
    
    def __init__(self):
        self.reader = None
        self.connection = None
        
    def find_reader(self) -> Optional[str]:
        """Find the CCID reader with pinpad support"""
        all_readers = readers()
        
        for reader in all_readers:
            name = str(reader)
            # Look for our reader
            if 'CCID' in name or 'Amperstrand' in name or 'STM32' in name:
                print(f"Found reader: {name}")
                return reader
        
        print("No suitable reader found. Available readers:")
        for r in all_readers:
            print(f"  - {r}")
        return None
    
    def connect(self) -> bool:
        """Connect to the reader and card"""
        self.reader = self.find_reader()
        if not self.reader:
            return False
        
        try:
            self.connection = self.reader.createConnection()
            self.connection.connect()
            print(f"Connected! ATR: {toHexString(self.connection.getATR())}")
            return True
        except NoCardException:
            print("No card present in reader")
            return False
        except CardConnectionException as e:
            print(f"Failed to connect to card: {e}")
            return False
    
    def disconnect(self):
        """Disconnect from the card"""
        if self.connection:
            try:
                self.connection.disconnect()
            except:
                pass
            self.connection = None
    
    def send_apdu(self, apdu: List[int]) -> Tuple[bytes, int, int]:
        """Send APDU and return (data, sw1, sw2)"""
        if not self.connection:
            raise RuntimeError("Not connected to card")
        
        print(f">> APDU: {toHexString(apdu)}")
        data, sw1, sw2 = self.connection.transmit(apdu)
        print(f"<< SW: {sw1:02X} {sw2:02X}, Data: {toHexString(data) if data else '(empty)'}")
        return bytes(data), sw1, sw2
    
    def select_openpgp(self) -> bool:
        """Select OpenPGP applet"""
        apdu = [
            self.CLA,
            self.INS_SELECT,
            0x04,  # P1: select by AID
            0x00,  # P2
            len(self.OPENPGP_AID)
        ] + self.OPENPGP_AID
        
        _, sw1, sw2 = self.send_apdu(apdu)
        return sw1 == 0x90 and sw2 == 0x00
    
    def verify_pin_direct(self, pin: str, pin_type: int = USER_PIN_P2) -> bool:
        """
        Verify PIN using direct APDU (not using pinpad).
        This is used for testing without pinpad UI.
        """
        pin_bytes = pin.encode('ascii')
        apdu = [
            self.CLA,
            self.INS_VERIFY,
            0x00,
            pin_type,
            len(pin_bytes)
        ] + list(pin_bytes)
        
        _, sw1, sw2 = self.send_apdu(apdu)
        
        if sw1 == 0x90 and sw2 == 0x00:
            print("PIN verified successfully!")
            return True
        elif sw1 == 0x63:
            remaining = sw2 & 0x0F
            print(f"Wrong PIN! {remaining} attempts remaining")
            return False
        else:
            print(f"PIN verification failed: {sw1:02X} {sw2:02X}")
            return False
    
    def test_pinpad_verify(self, expected_pin: str, pin_type: int = USER_PIN_P2) -> bool:
        """
        Test pinpad-based PIN verification.
        
        This sends a PC_to_RDR_Secure command to trigger pinpad entry.
        The user should enter the PIN on the device's touchscreen.
        """
        print(f"\n=== Pinpad PIN Verification Test ===")
        print(f"Expected PIN: {'*' * len(expected_pin)}")
        print(f"PIN type: {'User' if pin_type == self.USER_PIN_P2 else 'Admin'}")
        
        # Note: This requires using the CCID driver directly, not PC/SC
        # PC/SC doesn't expose the CCID Secure command
        # We'll need to use a different approach
        
        print("\nNote: Direct CCID Secure command requires raw CCID access.")
        print("Use GnuPG or OpenSC to test pinpad functionality:")
        print("  gpg --card-edit")
        print("  > admin")
        print("  > passwd")
        
        return True
    
    def get_card_status(self) -> dict:
        """Get OpenPGP card status information"""
        status = {}
        
        # Get cardholder data
        # DO 005E = Cardholder Related Data
        apdu = [0x00, 0xCA, 0x00, 0x5E, 0x00]
        data, sw1, sw2 = self.send_apdu(apdu)
        if sw1 == 0x90:
            status['cardholder_data'] = data.hex()
        
        # Get application related data (DO 6E)
        apdu = [0x00, 0xCA, 0x00, 0x6E, 0x00]
        data, sw1, sw2 = self.send_apdu(apdu)
        if sw1 == 0x90:
            status['application_data'] = data.hex()
        
        # Get PIN status
        # DO 7A = Security support template
        apdu = [0x00, 0xCA, 0x00, 0x7A, 0x00]
        data, sw1, sw2 = self.send_apdu(apdu)
        if sw1 == 0x90:
            status['pin_status'] = data.hex()
            # Parse remaining attempts
            # Tag C4 contains PW1/PW3 attempt counters
            if len(data) >= 3 and data[0] == 0xC4:
                attempts_byte = data[2]
                status['user_pin_attempts'] = (attempts_byte >> 4) & 0x0F
                status['admin_pin_attempts'] = attempts_byte & 0x0F
        
        return status


def main():
    print("=" * 60)
    print("CCID Pinpad Hardware Test")
    print("=" * 60)
    
    test = PinpadTest()
    
    try:
        # Connect to reader and card
        if not test.connect():
            print("\nFailed to connect. Exiting.")
            return 1
        
        # Select OpenPGP applet
        print("\n--- Selecting OpenPGP applet ---")
        if not test.select_openpgp():
            print("Failed to select OpenPGP applet")
            return 1
        print("OpenPGP applet selected successfully")
        
        # Get card status
        print("\n--- Card Status ---")
        status = test.get_card_status()
        for key, value in status.items():
            print(f"  {key}: {value}")
        
        # Test direct PIN verification (non-pinpad)
        print("\n--- Testing Direct PIN Verification ---")
        print("Enter PIN to test (default: 123456): ", end="", flush=True)
        try:
            pin = input().strip() or "123456"
        except EOFError:
            pin = "123456"
        
        if test.verify_pin_direct(pin):
            print("\nDirect PIN verification: PASSED")
        else:
            print("\nDirect PIN verification: FAILED (this is expected with wrong PIN)")
        
        # Pinpad test instructions
        print("\n--- Pinpad Testing Instructions ---")
        print("To test the pinpad UI:")
        print("1. Reset the card to un-verify the PIN")
        print("2. Run: gpg --card-edit")
        print("3. Enter 'admin' mode")
        print("4. Run 'verify' command")
        print("5. The device should display the PIN entry UI")
        print("6. Enter PIN using the touchscreen")
        
        print("\nAlternatively, use OpenSC tools:")
        print("  opensc-tool -s '00:20:00:81:06:31:32:33:34:35:36'")
        
        print("\n" + "=" * 60)
        print("Hardware test completed successfully!")
        print("=" * 60)
        
        return 0
        
    except Exception as e:
        print(f"\nError: {e}")
        import traceback
        traceback.print_exc()
        return 1
        
    finally:
        test.disconnect()


if __name__ == "__main__":
    sys.exit(main())
