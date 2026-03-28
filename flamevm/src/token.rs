
/// Encrypted and proven to be in-range token, portable.
pub struct Token {

}

/// Encrypted, but unproven to be in-range token.
/// This is a super-type for Token and non-portable.
pub struct WideToken {

}

/// This is a unencrypted token. Portable if non-negative.
pub struct ClearToken {

}