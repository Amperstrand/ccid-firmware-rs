package javacard.security;

import com.licel.jcardsim.crypto.AsymmetricSignatureImpl;
import com.licel.jcardsim.crypto.SymmetricSignatureImpl;
import org.amperstrand.sim.EcdsaSha2Signature;

/**
 * Runtime SHADOW of jcardsim 2.2.2's javacard.security.Signature.
 *
 * Why this exists: jcardsim 2.2.2's getInstance switch only covers JC 2.2.2
 * algorithm constants 1..30. Real JC 3.0.x applets (SmartPGP among them)
 * request the ECDSA-SHA2 family — ALG_ECDSA_SHA_224=37, _256=33, _384=34,
 * _512=38 — which falls off the end of that switch and raises
 * CryptoException.NO_SUCH_ALGORITHM during the applet's install().
 *
 * This class replicates jcardsim 2.2.2's routing byte-for-byte for 1..30 and
 * adds 33/34/37/38 routed to {@link EcdsaSha2Signature} (host BouncyCastle,
 * the same crypto stack jcardsim itself uses for ALG_ECDSA_SHA).
 *
 * CLASSPATH RULE: it only takes effect if this compiled class precedes
 * jcardsim-2.2.2.jar on the runtime classpath (the vjavacard launch line in
 * test_vjavacard.py already orders target/classes before the jar).
 */
public abstract class Signature {

    // JC 2.2.2 constants (values per the Java Card 2.2.2 API spec)
    public static final byte ALG_DES_MAC4_NOPAD = 1;
    public static final byte ALG_DES_MAC8_NOPAD = 2;
    public static final byte ALG_DES_MAC4_ISO9797_M1 = 3;
    public static final byte ALG_DES_MAC8_ISO9797_M1 = 4;
    public static final byte ALG_DES_MAC4_ISO9797_M2 = 5;
    public static final byte ALG_DES_MAC8_ISO9797_M2 = 6;
    public static final byte ALG_DES_MAC4_PKCS5 = 7;
    public static final byte ALG_DES_MAC8_PKCS5 = 8;
    public static final byte ALG_RSA_SHA_ISO9796 = 9;
    public static final byte ALG_RSA_SHA_PKCS1 = 10;
    public static final byte ALG_RSA_MD5_PKCS1 = 11;
    public static final byte ALG_RSA_RIPEMD160_ISO9796 = 12;
    public static final byte ALG_RSA_RIPEMD160_PKCS1 = 13;
    public static final byte ALG_DSA_SHA = 14;
    public static final byte ALG_RSA_SHA_RFC2409 = 15;
    public static final byte ALG_RSA_MD5_RFC2409 = 16;
    public static final byte ALG_ECDSA_SHA = 17;
    public static final byte ALG_AES_MAC_128_NOPAD = 18;
    public static final byte ALG_DES_MAC4_ISO9797_1_M2_ALG3 = 19;
    public static final byte ALG_DES_MAC8_ISO9797_1_M2_ALG3 = 20;
    public static final byte ALG_RSA_SHA_PKCS1_PSS = 21;
    public static final byte ALG_RSA_MD5_PKCS1_PSS = 22;
    public static final byte ALG_RSA_RIPEMD160_PKCS1_PSS = 23;
    public static final byte ALG_HMAC_SHA1 = 24;
    public static final byte ALG_HMAC_SHA_256 = 25;
    public static final byte ALG_HMAC_SHA_384 = 26;
    public static final byte ALG_HMAC_SHA_512 = 27;
    public static final byte ALG_HMAC_MD5 = 28;
    public static final byte ALG_HMAC_RIPEMD160 = 29;
    public static final byte ALG_RSA_SHA_ISO9796_MR = 30;
    // JC 3.0.x additions routed by this shadow (values per the JC 3.0.x API)
    public static final byte ALG_ECDSA_SHA_256 = 33;
    public static final byte ALG_ECDSA_SHA_384 = 34;
    public static final byte ALG_ECDSA_SHA_224 = 37;
    public static final byte ALG_ECDSA_SHA_512 = 38;

    public static final byte MODE_SIGN = 1;
    public static final byte MODE_VERIFY = 2;

    protected Signature() {
    }

    public static final Signature getInstance(byte algorithm, boolean externalAccess)
            throws CryptoException {
        if (externalAccess) {
            CryptoException.throwIt(CryptoException.ILLEGAL_USE);
        }
        switch (algorithm) {
            // asymmetric — same set jcardsim 2.2.2 routes to AsymmetricSignatureImpl
            case ALG_RSA_SHA_ISO9796:
            case ALG_RSA_SHA_PKCS1:
            case ALG_RSA_MD5_PKCS1:
            case ALG_RSA_RIPEMD160_ISO9796:
            case ALG_RSA_RIPEMD160_PKCS1:
            case ALG_ECDSA_SHA:
            case ALG_RSA_SHA_ISO9796_MR:
                return new AsymmetricSignatureImpl(algorithm);
            // ECDSA-SHA2 family: jcardsim 2.2.2 lacks these — host BC shim
            case ALG_ECDSA_SHA_224:
            case ALG_ECDSA_SHA_256:
            case ALG_ECDSA_SHA_384:
            case ALG_ECDSA_SHA_512:
                return new EcdsaSha2Signature(algorithm);
            // symmetric — same set jcardsim 2.2.2 routes to SymmetricSignatureImpl
            case ALG_DES_MAC4_NOPAD:
            case ALG_DES_MAC8_NOPAD:
            case ALG_DES_MAC4_ISO9797_M1:
            case ALG_DES_MAC8_ISO9797_M1:
            case ALG_DES_MAC4_ISO9797_M2:
            case ALG_DES_MAC8_ISO9797_M2:
            case ALG_DES_MAC4_PKCS5:
            case ALG_DES_MAC8_PKCS5:
            case ALG_AES_MAC_128_NOPAD:
            case ALG_DES_MAC8_ISO9797_1_M2_ALG3:
            case ALG_HMAC_SHA1:
            case ALG_HMAC_SHA_256:
            case ALG_HMAC_SHA_384:
            case ALG_HMAC_SHA_512:
            case ALG_HMAC_MD5:
            case ALG_HMAC_RIPEMD160:
                return new SymmetricSignatureImpl(algorithm);
            // unsupported by jcardsim 2.2.2 upstream too
            default:
                CryptoException.throwIt(CryptoException.NO_SUCH_ALGORITHM);
                return null; // unreachable
        }
    }

    public abstract void init(Key theKey, byte theMode) throws CryptoException;

    public abstract void init(Key theKey, byte theMode, byte[] initialMessage,
            short initialSignLength) throws CryptoException;

    public abstract short getLength() throws CryptoException;

    public abstract byte getAlgorithm();

    public abstract void update(byte[] inBuff, short inOffset, short inLength)
            throws CryptoException;

    public abstract short sign(byte[] inBuff, short inOffset, short inLength,
            byte[] sigBuff, short sigOffset) throws CryptoException;

    public abstract boolean verify(byte[] inBuff, short inOffset, short inLength,
            byte[] sigBuff, short sigOffset, short sigLength) throws CryptoException;
}
