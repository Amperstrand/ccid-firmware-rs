package org.amperstrand.sim;

import java.security.MessageDigest;

import org.bouncycastle.crypto.Digest;
import org.bouncycastle.crypto.params.ECPrivateKeyParameters;
import org.bouncycastle.crypto.params.ECPublicKeyParameters;
import org.bouncycastle.crypto.signers.DSADigestSigner;
import org.bouncycastle.crypto.signers.ECDSASigner;

import com.licel.jcardsim.crypto.ECKeyImpl;

import javacard.framework.Util;
import javacard.security.CryptoException;
import javacard.security.Key;
import javacard.security.Signature;

/**
 * ECDSA-SHA2 family (ALG_ECDSA_SHA_224/256/384/512) for jcardsim 2.2.2 via
 * the BouncyCastle build jcardsim itself bundles (same stack as its
 * ALG_ECDSA_SHA support).
 *
 * Keys arrive as jcardsim's ECPrivateKeyImpl/ECPublicKeyImpl whose
 * getParameters() already exposes BouncyCastle parameters, so no EC math is
 * reimplemented here. Signatures are the DER SEQUENCE{r,s} encoding Java
 * Card specifies. SHA-224 has no digest in the bundled BC subset, so it is
 * bridged through the host JDK's MessageDigest.
 */
public final class EcdsaSha2Signature extends Signature {

    private final byte algorithm;
    private DSADigestSigner signer;
    private boolean signMode;
    private boolean initialized;

    public EcdsaSha2Signature(byte algorithm) {
        this.algorithm = algorithm;
    }

    /** Bridge the host JDK's MessageDigest into BC's Digest interface. */
    static final class JdkDigest implements Digest {
        private final MessageDigest md;

        JdkDigest(String algorithm) {
            try {
                this.md = MessageDigest.getInstance(algorithm);
            } catch (java.security.NoSuchAlgorithmException e) {
                throw new IllegalStateException(e);
            }
        }

        public String getAlgorithmName() {
            return md.getAlgorithm();
        }

        public int getDigestSize() {
            return md.getDigestLength();
        }

        public void update(byte in) {
            md.update(in);
        }

        public void update(byte[] in, int inOff, int len) {
            md.update(in, inOff, len);
        }

        public int doFinal(byte[] out, int outOff) {
            try {
                md.digest(out, outOff, md.getDigestLength());
                return md.getDigestLength();
            } catch (java.security.DigestException e) {
                throw new IllegalStateException(e);
            }
        }

        public void reset() {
            md.reset();
        }
    }

    private DSADigestSigner newSigner(byte algorithm) throws CryptoException {
        ECDSASigner dsa = new ECDSASigner();
        switch (algorithm) {
            case Signature.ALG_ECDSA_SHA_224:
                return new DSADigestSigner(dsa, new JdkDigest("SHA-224"));
            case Signature.ALG_ECDSA_SHA_256:
                return new DSADigestSigner(dsa, new org.bouncycastle.crypto.digests.SHA256Digest());
            case Signature.ALG_ECDSA_SHA_384:
                return new DSADigestSigner(dsa, new org.bouncycastle.crypto.digests.SHA384Digest());
            case Signature.ALG_ECDSA_SHA_512:
                return new DSADigestSigner(dsa, new org.bouncycastle.crypto.digests.SHA512Digest());
            default:
                CryptoException.throwIt(CryptoException.NO_SUCH_ALGORITHM);
                return null;
        }
    }

    private void initInternal(Key theKey, byte theMode) throws CryptoException {
        if (theMode != Signature.MODE_SIGN && theMode != Signature.MODE_VERIFY) {
            CryptoException.throwIt(CryptoException.ILLEGAL_VALUE);
        }
        if (!(theKey instanceof ECKeyImpl)) {
            CryptoException.throwIt(CryptoException.ILLEGAL_VALUE);
        }
        Object params = ((ECKeyImpl) theKey).getParameters();
        if (params == null) {
            CryptoException.throwIt(CryptoException.UNINITIALIZED_KEY);
        }
        this.signMode = theMode == Signature.MODE_SIGN;
        this.signer = newSigner(algorithm);
        if (signMode) {
            if (!(params instanceof ECPrivateKeyParameters)) {
                CryptoException.throwIt(CryptoException.ILLEGAL_VALUE);
            }
            signer.init(true, (org.bouncycastle.crypto.CipherParameters) params);
        } else {
            if (!(params instanceof ECPublicKeyParameters)) {
                CryptoException.throwIt(CryptoException.ILLEGAL_VALUE);
            }
            signer.init(false, (org.bouncycastle.crypto.CipherParameters) params);
        }
        this.initialized = true;
    }

    @Override
    public void init(Key theKey, byte theMode) throws CryptoException {
        initInternal(theKey, theMode);
    }

    @Override
    public void init(Key theKey, byte theMode, byte[] initialMessage, short initialSignLength)
            throws CryptoException {
        if (initialSignLength != 0) {
            CryptoException.throwIt(CryptoException.ILLEGAL_USE);
        }
        initInternal(theKey, theMode);
    }

    @Override
    public short getLength() throws CryptoException {
        // DER{r,s} worst case for a 521-bit curve ~ 2*66 + 10 header bytes
        return (short) 141;
    }

    @Override
    public byte getAlgorithm() {
        return algorithm;
    }

    @Override
    public void update(byte[] inBuff, short inOffset, short inLength) throws CryptoException {
        if (!initialized) {
            CryptoException.throwIt(CryptoException.ILLEGAL_USE);
        }
        signer.update(inBuff, inOffset & 0xFFFF, inLength & 0xFFFF);
    }

    @Override
    public short sign(byte[] inBuff, short inOffset, short inLength, byte[] sigBuff,
            short sigOffset) throws CryptoException {
        if (!initialized || !signMode) {
            CryptoException.throwIt(CryptoException.ILLEGAL_USE);
        }
        update(inBuff, inOffset, inLength);
        byte[] der = signer.generateSignature();
        Util.arrayCopyNonAtomic(der, (short) 0, sigBuff, sigOffset, (short) der.length);
        return (short) der.length;
    }

    @Override
    public boolean verify(byte[] inBuff, short inOffset, short inLength, byte[] sigBuff,
            short sigOffset, short sigLength) throws CryptoException {
        if (!initialized || signMode) {
            CryptoException.throwIt(CryptoException.ILLEGAL_USE);
        }
        update(inBuff, inOffset, inLength);
        byte[] der = new byte[sigLength & 0xFFFF];
        Util.arrayCopyNonAtomic(sigBuff, sigOffset, der, (short) 0, sigLength);
        return signer.verifySignature(der);
    }
}
