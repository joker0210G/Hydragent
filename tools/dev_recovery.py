#!/usr/bin/env python3
"""
Hydragent Vault Emergency Developer Recovery Tool
=================================================
This is a self-contained offline script for the Developer to run
when a user requests vault recovery.

Requirements:
    pip install cryptography

Usage:
    python tools/dev_recovery.py
"""

import sys
import json
import hashlib
import os

try:
    from cryptography.hazmat.primitives.asymmetric import x25519, ed25519
    from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305
except ImportError:
    print("Error: The 'cryptography' library is required to run this script.")
    print("Please install it by running: pip install cryptography")
    sys.exit(1)

def parse_dev_private_key_file(contents: str):
    for line in contents.splitlines():
        line = line.strip()
        if line.startswith("#") or not line:
            continue
        if line.startswith("DEVELOPER_PRIVATE_KEY="):
            hex_val = line.split("=", 1)[1].strip()
            bytes_val = bytes.fromhex(hex_val)
            if len(bytes_val) < 32:
                raise ValueError("DEVELOPER_PRIVATE_KEY must be at least 32 bytes")
            return bytes_val[:32]
    raise ValueError("DEVELOPER_PRIVATE_KEY line not found")

def main():
    print("==================================================================")
    print("🐉 Hydragent Offline Emergency Recovery Tool (Developer Side)")
    print("==================================================================\n")

    # 1. Load Developer Private Key
    dev_key_path = input("Enter path to offline 'dev_private_key.txt' (default: config/security/dev_private_key.txt): ").strip()
    if not dev_key_path:
        dev_key_path = "config/security/dev_private_key.txt"

    if not os.path.exists(dev_key_path):
        print(f"Error: File not found at {dev_key_path}")
        sys.exit(1)

    try:
        with open(dev_key_path, "r") as f:
            dev_priv_bytes = parse_dev_private_key_file(f.read())
    except Exception as e:
        print(f"Error reading/parsing developer private key file: {e}")
        sys.exit(1)

    # 2. Input registered user Recovery Identity Public Key (id_pub)
    id_pub_hex = input("\nEnter user's registered Recovery Identity Public Key (id_pub hex): ").strip()
    try:
        id_pub_bytes = bytes.fromhex(id_pub_hex)
        if len(id_pub_bytes) != 32:
            raise ValueError("Public key must be exactly 32 bytes (64 hex characters)")
        user_id_pub = ed25519.Ed25519PublicKey.from_public_bytes(id_pub_bytes)
    except Exception as e:
        print(f"Error parsing user's id_pub: {e}")
        sys.exit(1)

    # 3. Input JSON recovery request
    print("\nPaste the User Recovery Request JSON below (ends with a blank line):")
    lines = []
    while True:
        line = sys.stdin.readline()
        if not line or line.strip() == "":
            break
        lines.append(line)
    
    request_str = "".join(lines).strip()
    if not request_str:
        print("Error: Empty request JSON")
        sys.exit(1)

    try:
        payload = json.loads(request_str)
        request_dict = payload["request"]
        signature_hex = payload["signature"]
        signature_bytes = bytes.fromhex(signature_hex)
    except Exception as e:
        print(f"Error parsing request JSON: {e}")
        sys.exit(1)

    # 4. Verify signature over the canonical request dictionary representation
    try:
        # Reconstruct canonical sorted request bytes for verification
        canonical_req = json.dumps(request_dict, sort_keys=True, separators=(',', ':')).encode('utf-8')
        user_id_pub.verify(signature_bytes, canonical_req)
        print("\n✅ Signature VERIFIED. Request is authentic.")
    except Exception as e:
        print(f"\n❌ Signature VERIFICATION FAILED: {e}")
        print("The request may have been tampered with or is not from the registered user.")
        sys.exit(1)

    # 5. Extract fields
    try:
        user_pub_hex = request_dict["user_public_key"]
        slot_2_ciphertext_hex = request_dict["slot_2_ciphertext"]
        slot_2_nonce_hex = request_dict["slot_2_nonce"]
        pub_r_hex = request_dict["pub_r"]
        timestamp = request_dict["timestamp"]
        nonce = request_dict["nonce"]

        user_pub_bytes = bytes.fromhex(user_pub_hex)
        slot_2_ciphertext = bytes.fromhex(slot_2_ciphertext_hex)
        slot_2_nonce = bytes.fromhex(slot_2_nonce_hex)
        pub_r_bytes = bytes.fromhex(pub_r_hex)
    except Exception as e:
        print(f"Error extracting request fields: {e}")
        sys.exit(1)

    print(f"Request timestamp: {timestamp}")
    print(f"Request nonce: {nonce}")

    # 6. Derive Slot2Key via X25519 DH key exchange
    try:
        dev_priv = x25519.X25519PrivateKey.from_private_bytes(dev_priv_bytes)
        user_pub = x25519.X25519PublicKey.from_public_bytes(user_pub_bytes)
        shared_secret = dev_priv.exchange(user_pub)
        slot2_key = hashlib.sha256(shared_secret).digest()
    except Exception as e:
        print(f"Error performing Slot 2 DH exchange: {e}")
        sys.exit(1)

    # 7. Decrypt Slot 2 Master Key
    try:
        cipher = ChaCha20Poly1305(slot2_key)
        master_key = cipher.decrypt(slot_2_nonce, slot_2_ciphertext, None)
        print("🔓 Successfully decrypted vault MasterKey.")
    except Exception as e:
        print(f"❌ Failed to decrypt Slot 2 MasterKey: {e}")
        print("Ensure the correct developer key and user public key were used.")
        sys.exit(1)

    # 8. Derive SessionKey via X25519 DH key exchange with ephemeral pub_r
    try:
        pub_r = x25519.X25519PublicKey.from_public_bytes(pub_r_bytes)
        session_shared = dev_priv.exchange(pub_r)
        session_key = hashlib.sha256(session_shared).digest()
    except Exception as e:
        print(f"Error performing Session DH exchange: {e}")
        sys.exit(1)

    # 9. Encrypt Master Key under SessionKey to create WrappedResponse
    try:
        session_cipher = ChaCha20Poly1305(session_key)
        response_nonce = os.urandom(12)
        wrapped_master_key = session_cipher.encrypt(response_nonce, master_key, None)
        
        # Zero out master_key from memory (as best as Python allows)
        # (Overwriting bytes in-place)
        bytearray_mk = bytearray(master_key)
        for i in range(len(bytearray_mk)):
            bytearray_mk[i] = 0
    except Exception as e:
        print(f"Error wrapping response: {e}")
        sys.exit(1)

    # 10. Generate and output WrappedResponse JSON
    wrapped_response = {
        "ciphertext": wrapped_master_key.hex(),
        "nonce": response_nonce.hex()
    }

    print("\n==================================================================")
    print("Wrapped Response JSON (Send this back to the user):")
    print("==================================================================")
    print(json.dumps(wrapped_response, indent=2))
    print("==================================================================")

if __name__ == "__main__":
    main()
