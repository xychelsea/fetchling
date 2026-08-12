//! Configuration defaults, `.netrc` credentials, and `.wgetrc` / `-e` commands.

mod defaults;
mod netrc;
mod wgetrc;

pub use defaults::Config;
pub use netrc::{lookup_credentials, parse_netrc, Netrc, NetrcEntry};
pub use wgetrc::{apply_wgetrc_command, load_wgetrc_files};
