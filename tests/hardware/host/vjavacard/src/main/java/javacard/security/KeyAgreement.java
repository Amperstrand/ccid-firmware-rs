package javacard.security;

import org.amperstrand.sim.EcdhPlainKeyAgreement;
import com.licel.jcardsim.crypto.KeyAgreementImpl;

/**
 * Runtime SHADOW of jcardsim 2.2.2's javacard.security.KeyAgreement.
 *
 * jcardsim 2.2.2 only implements ALG_EC_SVDP_DH(1)/ALG_EC_SVDP_DHC(2).
 * JC 3.0.x applets (SmartPGP) use ALG_EC_SVDP_DH_PLAIN(3) — raw x-coordinate
 * ECDH, no hash — which KeyAgreementImpl rejects with NO_SUCH_ALGORITHM.
 * This shadow keeps the jcardsim routing for 1/2 and adds 3 via
 * {@link EcdhPlainKeyAgreement} (BouncyCastle ECDH, same stack jcardsim
 * uses for the DH variants).
 *
 * CLASSPATH RULE: only effective when this class precedes jcardsim-2.2.2.jar
 * (unsigned copy — see the shadow Signature note) on the runtime classpath.
 */
public abstract class KeyAgreement {

    public static final byte ALG_EC_SVDP_DH = 1;
    public static final byte ALG_EC_SVDP_DHC = 2;
    public static final byte ALG_EC_SVDP_DH_PLAIN = 3;

    protected KeyAgreement() {
    }

    public static final KeyAgreement getInstance(byte algorithm, boolean externalAccess)
            throws CryptoException {
        if (externalAccess) {
            CryptoException.throwIt(CryptoException.ILLEGAL_USE);
        }
        switch (algorithm) {
            case ALG_EC_SVDP_DH:
            case ALG_EC_SVDP_DHC:
                return new KeyAgreementImpl(algorithm);
            case ALG_EC_SVDP_DH_PLAIN:
                return new EcdhPlainKeyAgreement(algorithm);
            default:
                CryptoException.throwIt(CryptoException.NO_SUCH_ALGORITHM);
                return null; // unreachable
        }
    }

    public abstract void init(PrivateKey privateKey) throws CryptoException;

    public abstract byte getAlgorithm();

    public abstract short generateSecret(byte[] inBuff, short inOffset, short inLength,
            byte[] outBuff, short secretOffset) throws CryptoException;
}
