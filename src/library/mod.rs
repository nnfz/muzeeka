// Library scanning and metadata module

pub mod database;
pub mod metadata;
pub mod scanner;

// Re-export main types
pub use database::LibraryDatabase;
pub use scanner::{LibraryScanner, MusicFile};
