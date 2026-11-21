#![forbid(unsafe_code)]
//! Shared, provider-independent identities and primitive contracts.
use std::fmt;
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityError { Empty, TooLong { maximum: usize }, ControlCharacter }

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("identifier must not be empty"),
            Self::TooLong { maximum } => write!(f, "identifier exceeds {maximum} bytes"),
            Self::ControlCharacter => f.write_str("identifier contains a control character"),
        }
    }
}
impl std::error::Error for IdentityError {}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Identifier(String);
impl Identifier {
    pub fn new(value: impl Into<String>, maximum: usize) -> Result<Self, IdentityError> {
        let value = value.into();
        if value.is_empty() { return Err(IdentityError::Empty); }
        if value.len() > maximum { return Err(IdentityError::TooLong { maximum }); }
        if value.chars().any(char::is_control) { return Err(IdentityError::ControlCharacter); }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str { &self.0 }
    pub fn into_string(self) -> String { self.0 }
}
impl fmt::Display for Identifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
}

macro_rules! identifier_type {
    ($name:ident, $maximum:expr) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Identifier);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, IdentityError> {
                Identifier::new(value, $maximum).map(Self)
            }
            pub fn as_str(&self) -> &str { self.0.as_str() }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }
        impl FromStr for $name {
            type Err = IdentityError;
            fn from_str(value: &str) -> Result<Self, Self::Err> { Self::new(value) }
        }
    };
}
identifier_type!(RunId, 256);
identifier_type!(ThreadId, 256);
identifier_type!(NodeId, 256);
identifier_type!(CheckpointId, 256);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SuperStep(pub u64);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_reject_empty_control_and_oversized_values() {
        assert_eq!(NodeId::new(""), Err(IdentityError::Empty));
        assert_eq!(NodeId::new("node\n1"), Err(IdentityError::ControlCharacter));
        assert_eq!(NodeId::new("x".repeat(257)), Err(IdentityError::TooLong { maximum: 256 }));
    }
    #[test]
    fn identifiers_have_stable_order_and_display() {
        let alpha = NodeId::new("alpha");
        let beta = NodeId::new("beta");
        assert!(alpha.is_ok() && beta.is_ok());
        if let (Ok(alpha), Ok(beta)) = (alpha, beta) {
            assert!(alpha < beta);
            assert_eq!(alpha.to_string(), "alpha");
        }
    }
}
