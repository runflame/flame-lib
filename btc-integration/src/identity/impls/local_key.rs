use bitcoin::secp256k1::{Message, PublicKey, Secp256k1, SecretKey, ecdsa};
use rand::rngs::OsRng;

use crate::identity::{IdentityKey, IdentityKeyError};

pub struct LocalKey {
    secret: SecretKey,
    public_key: PublicKey,
}

impl LocalKey {
    /// The key of `secret`. The caller still has its own copy of the secret
    /// and has to erase it.
    pub fn new(secret: SecretKey) -> Self {
        let public_key = secret.public_key(&Secp256k1::signing_only());
        LocalKey { secret, public_key }
    }

    pub fn generate() -> Self {
        LocalKey::new(SecretKey::new(&mut OsRng))
    }
}

impl Drop for LocalKey {
    fn drop(&mut self) {
        self.secret.non_secure_erase();
    }
}

impl IdentityKey for LocalKey {
    fn public_key(&self) -> PublicKey {
        self.public_key
    }

    async fn sign(&self, digest: Message) -> Result<ecdsa::Signature, IdentityKeyError> {
        Ok(Secp256k1::signing_only().sign_ecdsa(&digest, &self.secret))
    }
}
