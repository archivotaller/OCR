//! Diccionarios de caracteres (charset) embebidos para el decodificador CTC.
//!
//! Los diccionarios provienen de PaddleOCR (Apache-2.0):
//!   - tiny:  6.904 caracteres (chino + inglés + latín básico)
//!   - small/medium: 18.708 caracteres (50 idiomas, incluye japonés)
//!
//! El token de índice 0 (blank) y el espacio final los añade `Charset`.

use crate::models::Tier;

pub fn charset_for(tier: Tier) -> &'static str {
    match tier {
        Tier::Tiny => include_str!("../assets/charset_tiny.txt"),
        Tier::Small | Tier::Medium => include_str!("../assets/charset_medium.txt"),
    }
}

/// Número de caracteres del diccionario (sin blank ni espacio).
pub fn charset_len(tier: Tier) -> usize {
    charset_for(tier).lines().count()
}
