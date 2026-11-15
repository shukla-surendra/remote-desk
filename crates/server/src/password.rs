//! Verification of agent-supplied Argon2 hashes.
//!
//! The hash string comes from the agent, so it is untrusted: a malicious agent could
//! advertise enormous cost parameters to make the server burn CPU/RAM on every login
//! attempt. We only accept argon2id with bounded parameters.

use argon2::{password_hash::PasswordHash, Argon2, PasswordVerifier};

const MAX_M_COST_KIB: u32 = 64 * 1024;
const MAX_T_COST: u32 = 4;
const MAX_P_COST: u32 = 4;

/// Returns `Err` with a reason if the PHC string is not an acceptable argon2id hash.
pub fn validate_hash(phc: &str) -> Result<(), &'static str> {
    let parsed = PasswordHash::new(phc).map_err(|_| "malformed password hash")?;
    if parsed.algorithm.as_str() != "argon2id" {
        return Err("password hash must be argon2id");
    }
    let params = argon2::Params::try_from(&parsed).map_err(|_| "invalid argon2 params")?;
    if params.m_cost() > MAX_M_COST_KIB
        || params.t_cost() > MAX_T_COST
        || params.p_cost() > MAX_P_COST
    {
        return Err("argon2 parameters exceed server limits");
    }
    Ok(())
}

/// CPU-heavy; call from `spawn_blocking`.
pub fn verify(phc: &str, password: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(h) => Argon2::default()
            .verify_password(password.as_bytes(), &h)
            .is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use argon2::{
        password_hash::{rand_core::OsRng, SaltString},
        Algorithm, Params, PasswordHasher, Version,
    };

    fn hash(params: Params, alg: Algorithm) -> String {
        let salt = SaltString::generate(&mut OsRng);
        Argon2::new(alg, Version::V0x13, params)
            .hash_password(b"secret", &salt)
            .unwrap()
            .to_string()
    }

    #[test]
    fn accepts_default_argon2id() {
        let h = hash(Params::default(), Algorithm::Argon2id);
        assert!(validate_hash(&h).is_ok());
        assert!(verify(&h, "secret"));
        assert!(!verify(&h, "wrong"));
    }

    #[test]
    fn rejects_expensive_params() {
        let h = hash(Params::new(8, 50, 1, None).unwrap(), Algorithm::Argon2id);
        assert!(validate_hash(&h).is_err());
    }

    #[test]
    fn rejects_other_algorithms() {
        let h = hash(Params::default(), Algorithm::Argon2i);
        assert!(validate_hash(&h).is_err());
        assert!(validate_hash("not a hash").is_err());
    }
}
