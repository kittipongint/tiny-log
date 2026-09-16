use crate::error::{AppError, AppResult};
use argon2::password_hash::{phc::PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};

const MIN_PASSWORD_LEN: usize = 12;

pub fn validate_password_strength(password: &str) -> AppResult<()> {
    if password.len() < MIN_PASSWORD_LEN {
        return Err(AppError::bad_request(format!(
            "password must be at least {MIN_PASSWORD_LEN} characters"
        )));
    }
    Ok(())
}

fn argon2() -> AppResult<Argon2<'static>> {
    // Spec: Argon2id, v19, 64 MiB, 3 iterations, parallelism 2, 32-byte output
    let params = Params::new(65536, 3, 2, Some(32))
        .map_err(|e| AppError::internal(format!("argon2 params: {e}")))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

pub fn hash_password(password: &str) -> AppResult<String> {
    validate_password_strength(password)?;
    let hash = argon2()?
        .hash_password(password.as_bytes())
        .map_err(|e| AppError::internal(format!("hash failed: {e}")))?
        .to_string();
    Ok(hash)
}

pub fn verify_password(password: &str, password_hash: &str) -> AppResult<bool> {
    let parsed = PasswordHash::new(password_hash)
        .map_err(|_| AppError::internal("invalid stored password hash"))?;
    Ok(argon2()?
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}
