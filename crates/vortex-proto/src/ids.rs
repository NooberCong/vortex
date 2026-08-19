//! Identifier newtypes. Transparent on the wire, distinct in the type system.

use serde::{Deserialize, Serialize};
use std::fmt;
use ts_rs::TS;

macro_rules! id_newtype {
    ($name:ident, $inner:ty, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS,
        )]
        #[ts(export)]
        #[serde(transparent)]
        pub struct $name(pub $inner);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<$inner> for $name {
            fn from(v: $inner) -> Self {
                Self(v)
            }
        }

        impl From<$name> for $inner {
            fn from(v: $name) -> Self {
                v.0
            }
        }
    };
}

id_newtype!(JobId, u64, "Monotonic, assigned by the daemon, stable across restarts.");
id_newtype!(TabId, i64, "The browser's tab id, carried through so the overlay can be addressed.");
