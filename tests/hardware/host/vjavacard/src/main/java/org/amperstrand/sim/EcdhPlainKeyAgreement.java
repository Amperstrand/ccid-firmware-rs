package org.amperstrand.sim;

import java.math.BigInteger;

import org.bouncycastle.crypto.agreement.ECDHBasicAgreement;
import org.bouncycastle.crypto.params.ECDomainParameters;
import org.bouncycastle.crypto.params.ECPrivateKeyParameters;
import org.bouncycastle.crypto.params.ECPublicKeyParameters;
import org.bouncycastle.math.ec.ECCurve;
import org.bouncycastle.math.ec.ECPoint;

import com.licel.jcardsim.crypto.ECKeyImpl;

import javacard.security.CryptoException;
import javacard.security.KeyAgreement;
import javacard.security.PrivateKey;

/**
 * ALG_EC_SVDP_DH_PLAIN for jcardsim 2.2.2 — plain (unhashed) x-coordinate
 * ECDH over the BouncyCastle build jcardsim bundles.
 *
 * init() takes the jcardsim ECPrivateKeyImpl (which already carries BC
 * domain parameters); generateSecret() receives the peer public point in
 * Java Card W-format (uncompressed 0x04||X||Y) and writes the shared secret
 * as the fixed-length big-endian x-coordinate.
 */
public final class EcdhPlainKeyAgreement extends KeyAgreement {

    private final byte algorithm;
    private ECPrivateKeyParameters privParams;
    private ECDHBasicAgreement engine;

    public EcdhPlainKeyAgreement(byte algorithm) {
        this.algorithm = algorithm;
    }

    @Override
    public void init(PrivateKey privateKey) throws CryptoException {
        if (!(privateKey instanceof ECKeyImpl)) {
            CryptoException.throwIt(CryptoException.ILLEGAL_VALUE);
        }
        Object params = ((ECKeyImpl) privateKey).getParameters();
        if (!(params instanceof ECPrivateKeyParameters)) {
            CryptoException.throwIt(CryptoException.UNINITIALIZED_KEY);
        }
        this.privParams = (ECPrivateKeyParameters) params;
        this.engine = new ECDHBasicAgreement();
        this.engine.init(privParams);
    }

    @Override
    public byte getAlgorithm() {
        return algorithm;
    }

    @Override
    public short generateSecret(byte[] inBuff, short inOffset, short inLength,
            byte[] outBuff, short secretOffset) throws CryptoException {
        if (engine == null || privParams == null) {
            CryptoException.throwIt(CryptoException.ILLEGAL_USE);
        }
        if (inLength < 0 || inBuff[inOffset] != 0x04) {
            CryptoException.throwIt(CryptoException.ILLEGAL_VALUE);
        }
        ECDomainParameters dom = privParams.getParameters();
        ECCurve curve = dom.getCurve();
        try {
            ECPoint pub = curve.decodePoint(
                    java.util.Arrays.copyOfRange(inBuff, inOffset, inOffset + inLength));
            BigInteger secret = engine.calculateAgreement(
                    new ECPublicKeyParameters(pub, dom));
            int fieldLen = (curve.getFieldSize() + 7) / 8;
            byte[] out = positiveFixedLength(secret, fieldLen);
            System.arraycopy(out, 0, outBuff, secretOffset, out.length);
            return (short) out.length;
        } catch (IllegalArgumentException | ArithmeticException e) {
            throw new CryptoException(CryptoException.ILLEGAL_VALUE);
        }
    }

    private static byte[] positiveFixedLength(BigInteger v, int len) {
        byte[] raw = v.toByteArray(); // may carry a leading sign byte
        byte[] out = new byte[len];
        if (raw.length == len + 1) {
            System.arraycopy(raw, 1, out, 0, len);
        } else if (raw.length <= len) {
            System.arraycopy(raw, 0, out, len - raw.length, raw.length);
        } else {
            throw new IllegalStateException("secret longer than field");
        }
        return out;
    }
}
