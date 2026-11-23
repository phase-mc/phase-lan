//! Binding of an in-game nickname to a device in the room.
//!
//! When the host lets players in without an account check, the game server believes the nickname
//! the guest's game gave. So a nickname is tied to the device (the tunnel key ID)
//! that first joined this room's world with it; the host's nickname is always tied to the host.
//! A join with a taken nickname from another device is rejected in the tunnel already, so such a connection
//! never reaches the host's game. The binding lives while the room is open.
//!
//! With an account check, the nickname is confirmed by the Mojang service and no binding is needed: the same
//! player may move from a laptop to a desktop.

use std::collections::HashMap;

/// Why a nickname is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// This is the host's nickname.
    HostName,
    /// The nickname is already tied to another device of this room.
    Taken,
}

impl Refusal {
    /// Reason for the host's log.
    pub fn log_text(&self, name: &str) -> String {
        match self {
            Self::HostName => phase_lang::tr!(
                format!("ник {name} — ник хоста этой комнаты"),
                format!("the name {name} belongs to this room's host")
            ),
            Self::Taken => phase_lang::tr!(
                format!("ник {name} уже вошёл в эту комнату с другого устройства"),
                format!("the name {name} already joined this room from another device")
            ),
        }
    }

    /// Reason on the guest's game screen. The text is written by the host, so it is in the host's language.
    pub fn game_text(&self, name: &str) -> String {
        match self {
            Self::HostName => phase_lang::tr!(
                format!("Ник {name} занят хостом комнаты. Зайдите под другим ником."),
                format!("The name {name} is taken by the room's host. Join with a different name.")
            ),
            Self::Taken => phase_lang::tr!(
                format!(
                    "Ник {name} уже используется в этой комнате с другого устройства. Зайдите под другим ником."
                ),
                format!(
                    "The name {name} is already in use in this room from another device. Join with a different name."
                )
            ),
        }
    }
}

/// Who joined the room's world with which nickname. Nicknames are compared case-insensitively, as in
/// the game itself.
#[derive(Debug, Default)]
pub(crate) struct NameBindings {
    owners: HashMap<String, String>,
}

impl NameBindings {
    /// Whether device `device` may join under nickname `name`; on success the nickname
    /// is tied to it. `host_name` is the host's game nickname (if known).
    pub fn admit(
        &mut self,
        device: &str,
        name: &str,
        host_name: Option<&str>,
    ) -> Result<(), Refusal> {
        if host_name.is_some_and(|host| host.eq_ignore_ascii_case(name)) {
            return Err(Refusal::HostName);
        }
        let key = name.to_ascii_lowercase();
        match self.owners.get(&key) {
            Some(owner) if owner != device => Err(Refusal::Taken),
            Some(_) => Ok(()),
            None => {
                self.owners.insert(key, device.to_string());
                Ok(())
            }
        }
    }

    /// Which device the nickname is tied to.
    #[cfg(test)]
    pub fn owner(&self, name: &str) -> Option<&str> {
        self.owners
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaa";
    const B: &str = "bbbb";

    #[test]
    fn first_device_keeps_the_name() {
        let mut names = NameBindings::default();
        assert_eq!(names.admit(A, "Steve_01", Some("Host")), Ok(()));
        assert_eq!(names.owner("steve_01"), Some(A));
        // The same device rejoins: allowed, any number of times.
        assert_eq!(names.admit(A, "Steve_01", Some("Host")), Ok(()));
        // Another device with the same nickname, in any case: no.
        assert_eq!(
            names.admit(B, "Steve_01", Some("Host")),
            Err(Refusal::Taken)
        );
        assert_eq!(
            names.admit(B, "STEVE_01", Some("Host")),
            Err(Refusal::Taken)
        );
        // The binding does not pass to another device even if the first left: the room remembers.
        assert_eq!(names.owner("Steve_01"), Some(A));
        // The second device gets its own nickname.
        assert_eq!(names.admit(B, "Alex", Some("Host")), Ok(()));
        // One device may also join under a second nickname (a second account on the same PC).
        assert_eq!(names.admit(A, "Steve_02", Some("Host")), Ok(()));
    }

    #[test]
    fn host_name_is_always_the_hosts() {
        let mut names = NameBindings::default();
        assert_eq!(names.admit(A, "Host", Some("Host")), Err(Refusal::HostName));
        assert_eq!(names.admit(A, "hOsT", Some("Host")), Err(Refusal::HostName));
        // A refusal does not tie the nickname to anyone.
        assert_eq!(names.owner("Host"), None);
        // The host's nickname is not known yet: the binding works by the general rule.
        assert_eq!(names.admit(A, "Host", None), Ok(()));
        assert_eq!(names.admit(B, "Host", None), Err(Refusal::Taken));
    }

    #[test]
    fn refusal_texts_name_the_nick() {
        for refusal in [Refusal::HostName, Refusal::Taken] {
            assert!(refusal.log_text("Kira").contains("Kira"));
            let shown = refusal.game_text("Kira");
            assert!(shown.contains("Kira") && shown.contains("другим ником"));
            // Neutral words: no "pirate" or "license" (in either language).
            for word in ["пират", "лиценз", "куп"] {
                assert!(!shown.to_lowercase().contains(word), "{shown}");
            }
        }
    }
}
