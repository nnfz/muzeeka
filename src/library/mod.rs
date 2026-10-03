// Library scanning and metadata module

pub mod scanner;
pub mod metadata;
pub mod database;

// Re-export main types
pub use scanner::{LibraryScanner, MusicFile};
pub use database::LibraryDatabase;
