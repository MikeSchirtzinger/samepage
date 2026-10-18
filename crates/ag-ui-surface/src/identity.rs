//! Who someone is, as distinct from what they are allowed to do.
//!
//! [`Caller`](crate::Caller) answers authorization and is derived from the
//! transport a call arrived on, so nothing a caller sends can change it. This
//! module answers attribution and is derived from a join. Keeping them apart is
//! what lets a surface record *which* person wrote something without changing
//! who may write, and it is why nothing here may ever be read for permission.
//!
//! Three lifetimes are involved, and collapsing any two of them loses something
//! the record needs:
//!
//! | | what it is | lives for |
//! |---|---|---|
//! | [`Principal`] | who someone is | forever |
//! | [`Person::participant_id`] | who they are *here* | the meeting |
//! | the resume token | this browser, this connection | one client |
//!
//! A byline stores the participant id. The session or connection underneath it
//! is deliberately never stored: an agent that drops and reconnects, or a person
//! who reloads the page, is one participant who did two things, not two
//! participants who each did one. A record that cannot tell those apart cannot
//! answer the only question this surface exists to answer.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Distinct, well-separated hues. A principal picks its preferred slot from a
/// hash so a person keeps their colour across sessions, and the room hands out
/// the first free one from there so two people are never the same colour even
/// when the hash would have collided.
///
/// Colour is computed here and shipped, never recomputed in the browser. The
/// same value derived independently in two languages is a drift bug waiting to
/// happen, and this one would show up as two people quietly sharing an identity.
const PALETTE: &[u16] = &[210, 28, 145, 320, 55, 265, 178, 12, 96, 300, 235, 40];

/// Who someone is, at whatever strength the tier can actually prove.
///
/// Both variants are the same shape to everything downstream — a byline, a
/// colour, a responsibility link — and differ only in what the host checked
/// before minting one. That is what keeps the open tier open: a room nobody was
/// invited to still gets working bylines, and a meeting that requires an
/// invitation gets the same bylines with a verified address behind them. The
/// tier changes how much the identity is worth, not how identity works.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Principal {
    /// Minted for a browser that simply showed up. It proves continuity across
    /// a reload and nothing else: not who anybody is, and not that the same
    /// human is still holding the laptop.
    Local { id: String },
    /// An address the host invited and the joiner authenticated as. Required at
    /// the meetings tier, never asked for below it.
    Invited { email: String },
}

impl Principal {
    /// The stable string a colour, a responsibility link, and an invitation are
    /// all keyed by. Prefixed by kind so a local id can never be mistaken for a
    /// verified address, whatever it was minted as.
    pub fn key(&self) -> String {
        match self {
            Principal::Local { id } => format!("local:{id}"),
            Principal::Invited { email } => format!("mailto:{}", email.to_ascii_lowercase()),
        }
    }

    /// Where in [`PALETTE`] this principal would prefer to sit.
    ///
    /// FNV-1a rather than [`std::collections::hash_map::DefaultHasher`], whose
    /// output is explicitly not stable across releases — a colour that silently
    /// changed on a Rust upgrade would read as a different person.
    fn preferred_slot(&self) -> usize {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self.key().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        (hash % PALETTE.len() as u64) as usize
    }

    /// The address to invite this principal at, when there is one. A local
    /// principal has no reachable address, which is exactly why the meetings
    /// tier refuses one.
    pub fn address(&self) -> Option<&str> {
        match self {
            Principal::Local { .. } => None,
            Principal::Invited { email } => Some(email),
        }
    }
}

/// One person in the room: a stable identity, the name to show for them, and
/// the colour that tells two same-named people apart.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    /// Host-minted and stable for as long as this person is in the room. This
    /// is the value a byline stores, and the value a viewer compares against
    /// its own to decide whether a mark says "you" or says a name.
    pub participant_id: String,
    pub principal: Principal,
    /// Preferred name, disambiguated against everyone already here. Display
    /// only: it is never read for permission and never has to be unique to be
    /// correct, only to be useful.
    pub name: String,
    pub hue: u16,
}

/// Everyone currently in the room, and the tokens that let them come back.
///
/// Keyed by resume token rather than by participant id on purpose. The token is
/// the secret a client presents; the participant id is public and appears on
/// every byline, so a registry keyed by the public value would let anyone who
/// can read a byline claim the person who wrote it.
#[derive(Default)]
pub struct People {
    by_token: Mutex<HashMap<String, Person>>,
    /// Where the registry survives a restart, when the application gave one.
    path: Mutex<Option<std::path::PathBuf>>,
}

impl People {
    pub fn new() -> Self {
        Self::default()
    }

    /// Keep the registry in `path`, so a person's resume token and chosen
    /// name survive a host restart. People already in the file are loaded.
    /// The file holds resume tokens and is written owner-only, like the auth
    /// store beside it.
    pub fn persist_to(&self, path: impl Into<std::path::PathBuf>) -> Result<(), String> {
        let path = path.into();
        if path.exists() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let stored: HashMap<String, Person> = serde_json::from_str(&text)
                .map_err(|e| format!("invalid {}: {e}", path.display()))?;
            self.by_token.lock().extend(stored);
        }
        *self.path.lock() = Some(path);
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let Some(path) = self.path.lock().clone() else {
            return Ok(());
        };
        let text = serde_json::to_vec_pretty(&*self.by_token.lock()).map_err(|e| e.to_string())?;
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, text).map_err(|e| format!("cannot write {}: {e}", temp.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        std::fs::rename(&temp, &path).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    fn saved(&self) {
        if let Err(error) = self.save() {
            tracing::error!(%error, "joined-person registry was not saved");
        }
    }

    /// Resolve a presented resume token to the person who holds it.
    ///
    /// A token nobody minted resolves to nothing rather than to a fresh person,
    /// so a forged or stale cookie cannot mint identity by being presented.
    pub fn resolve(&self, token: &str) -> Option<Person> {
        self.by_token.lock().get(token).cloned()
    }

    /// Admit someone, returning their person record and the resume token to
    /// hand back.
    ///
    /// The participant id is minted here and never taken from the client, for
    /// the same reason an attaching agent's is: a caller that could choose its
    /// own id could take over one already present.
    pub fn admit(
        &self,
        principal: Principal,
        proposed_name: &str,
        taken: &[String],
    ) -> (Person, String) {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let participant_id = format!("person-{}", uuid::Uuid::new_v4().simple());
        let mut people = self.by_token.lock();
        let mut names: Vec<String> = taken.to_vec();
        names.extend(people.values().map(|person| person.name.clone()));
        let name = crate::unique_label(proposed_name, &names);
        let hue = free_hue(principal.preferred_slot(), people.values());
        let person = Person {
            participant_id,
            principal,
            name,
            hue,
        };
        people.insert(token.clone(), person.clone());
        drop(people);
        self.saved();
        (person, token)
    }

    /// Everyone here, for presence and for the browser's "who else is in this
    /// room" list.
    pub fn everyone(&self) -> Vec<Person> {
        let mut all: Vec<Person> = self.by_token.lock().values().cloned().collect();
        all.sort_by(|a, b| a.participant_id.cmp(&b.participant_id));
        all
    }

    /// Rename someone already admitted. Returns the name actually taken, which
    /// may be disambiguated away from what was asked for.
    pub fn rename(&self, token: &str, proposed: &str, taken: &[String]) -> Option<String> {
        let mut people = self.by_token.lock();
        let mut names: Vec<String> = taken.to_vec();
        names.extend(
            people
                .iter()
                .filter(|(key, _)| key.as_str() != token)
                .map(|(_, person)| person.name.clone()),
        );
        let name = crate::unique_label(proposed, &names);
        let person = people.get_mut(token)?;
        person.name = name.clone();
        drop(people);
        self.saved();
        Some(name)
    }
}

/// The first palette slot nobody is using, starting from the one this principal
/// prefers. Falls back to the preference itself once everyone is seated, since
/// a repeated colour beats refusing to admit someone over decoration.
fn free_hue<'a>(preferred: usize, present: impl Iterator<Item = &'a Person>) -> u16 {
    let taken: Vec<u16> = present.map(|person| person.hue).collect();
    for step in 0..PALETTE.len() {
        let hue = PALETTE[(preferred + step) % PALETTE.len()];
        if !taken.contains(&hue) {
            return hue;
        }
    }
    PALETTE[preferred]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_principals_colour_does_not_move_between_sessions() {
        let principal = Principal::Invited {
            email: "mike@example.com".to_string(),
        };
        let first = People::new().admit(principal.clone(), "Mike", &[]).0.hue;
        let second = People::new().admit(principal, "Mike", &[]).0.hue;
        assert_eq!(
            first, second,
            "a colour derived per-process is a different person each restart"
        );
    }

    #[test]
    fn an_address_is_matched_case_insensitively() {
        let lower = Principal::Invited {
            email: "mike@example.com".to_string(),
        };
        let shouted = Principal::Invited {
            email: "Mike@Example.COM".to_string(),
        };
        assert_eq!(
            lower.key(),
            shouted.key(),
            "one invitation must not admit two people"
        );
    }

    /// The case the colour exists for: two people who chose the same name.
    #[test]
    fn two_people_sharing_a_name_get_different_names_and_colours() {
        let people = People::new();
        let (first, _) = people.admit(
            Principal::Invited {
                email: "mike@a.com".to_string(),
            },
            "Mike",
            &[],
        );
        let (second, _) = people.admit(
            Principal::Invited {
                email: "mike@b.com".to_string(),
            },
            "Mike",
            &[],
        );
        assert_ne!(first.name, second.name);
        assert_ne!(first.hue, second.hue);
        assert_ne!(first.participant_id, second.participant_id);
    }

    /// The reload case. Presenting the token again is the same person; not
    /// presenting one is a stranger, not a free identity.
    #[test]
    fn a_resume_token_returns_the_same_participant_and_a_forged_one_returns_nobody() {
        let people = People::new();
        let (person, token) = people.admit(
            Principal::Local {
                id: "abc".to_string(),
            },
            "someone",
            &[],
        );
        assert_eq!(
            people.resolve(&token).map(|found| found.participant_id),
            Some(person.participant_id)
        );
        assert_eq!(people.resolve("not-a-real-token"), None);
    }

    #[test]
    fn a_persisted_registry_keeps_a_chosen_name_across_a_restart() {
        let path = std::env::temp_dir().join(format!(
            "people-{}-{}.json",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let before = People::new();
        before.persist_to(&path).unwrap();
        let (person, token) = before.admit(Principal::Local { id: "a".into() }, "someone", &[]);
        before.rename(&token, "Dana", &[]).unwrap();
        let after = People::new();
        after.persist_to(&path).unwrap();
        let back = after
            .resolve(&token)
            .expect("the resume token still resolves");
        assert_eq!(
            (back.participant_id, back.name.as_str()),
            (person.participant_id, "Dana")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_local_principal_has_no_address_to_invite() {
        assert_eq!(
            Principal::Local {
                id: "abc".to_string()
            }
            .address(),
            None,
            "the meetings tier refuses a local principal precisely because of this"
        );
    }
}
