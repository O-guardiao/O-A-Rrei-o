pub mod api_key_store;
pub mod auto_rotation;
pub mod client_registry;
pub mod keychain;
pub mod rotation;
pub mod scanner;
pub mod vault;

pub use api_key_store::ApiKeyStore;
pub use auto_rotation::{AutoRotator, KeyVersion, RotationEvent, RotationPolicy, RotatorEntry};
pub use client_registry::{ClientIdentity, ClientRecord, ClientRegistry};
pub use keychain::KeychainStore;
pub use rotation::{KeyMetadata, KeyRotator};
pub use scanner::{SecretFinding, SecretScanner, SecretSeverity};
pub use vault::{SecretEntry, SecretVault};
