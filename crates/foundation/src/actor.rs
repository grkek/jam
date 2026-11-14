//! The description of an actor to start.
//!
//! An `Actor` says what to run and how; it holds no identity. The table that starts it picks the
//! slot and returns the `LocalAddress`, so a caller cannot forge or predict an address.

use crate::{address::LocalAddress, configuration::Configuration, name::Name};

/// The code an actor runs.
#[derive(Clone, Copy, Debug)]
pub enum Image<'a> {
    /// A statically linked `AArch64` ELF64 executable. Borrowed only for the duration of the
    /// spawn: the loader copies the segments into frames and keeps no reference.
    Elf(&'a [u8]),
    /// A kernel-mode entry point, running at EL1 on its own kernel stack.
    Kernel(extern "C" fn() -> !),
}

/// Everything needed to start one actor.
#[derive(Debug)]
pub struct Actor<'a> {
    /// Lookup name, or `Name::UNNAMED`.
    name: Name,
    /// The resource budget and settings the actor runs under. Checked against the system ceiling.
    configuration: Configuration,
    /// The spawner, or `None` for actors the kernel starts itself.
    parent: Option<LocalAddress>,
    /// The code to run.
    image: Image<'a>,
}

impl<'a> Actor<'a> {
    /// Creates a root actor with no parent.
    ///
    /// Name, configuration and image are what every actor needs, so they are required.
    #[must_use]
    pub const fn new(name: Name, configuration: Configuration, image: Image<'a>) -> Self {
        Self {
            name,
            configuration,
            parent: None,
            image,
        }
    }

    /// Returns this actor with the given spawner recorded as its parent.
    #[must_use]
    pub const fn with_parent(mut self, parent: LocalAddress) -> Self {
        self.parent = Some(parent);
        self
    }

    /// Returns the actor's lookup name.
    #[must_use]
    pub const fn name(&self) -> &Name {
        &self.name
    }

    /// Returns the configuration the actor will run under.
    #[must_use]
    pub const fn configuration(&self) -> &Configuration {
        &self.configuration
    }

    /// Returns the spawner's address, or `None` for a kernel-started actor.
    #[must_use]
    pub const fn parent(&self) -> Option<LocalAddress> {
        self.parent
    }

    /// Returns the code to run.
    #[must_use]
    pub const fn image(&self) -> &Image<'a> {
        &self.image
    }

    /// Splits off the parts the table moves into a control block. The image is not included,
    /// because it is consumed while the address space is built.
    #[must_use]
    pub fn into_parts(self) -> (Name, Configuration, Option<LocalAddress>) {
        (self.name, self.configuration, self.parent)
    }
}
