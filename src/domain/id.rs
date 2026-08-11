use std::fmt::{self, Display, Formatter};
use std::str::FromStr;
use ulid::{DecodeError, Ulid};

macro_rules! typed_id {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(Ulid::generate())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = DecodeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }

        impl From<Ulid> for $name {
            fn from(value: Ulid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for Ulid {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    )+};
}

typed_id!(
    AgentId,
    RoomId,
    ConversationId,
    MessageId,
    WorkItemId,
    ResultId,
    PublishId,
    SessionBindingId,
    CheckpointId,
    MemoryId,
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    macro_rules! assert_id_roundtrip {
        ($($id:ty),+ $(,)?) => {$({
            let id = <$id>::new();
            let encoded = id.to_string();
            assert_eq!(encoded.len(), 26);
            assert_eq!(<$id>::from_str(&encoded).unwrap(), id);
            assert_eq!(<$id>::from_str(&encoded.to_lowercase()).unwrap().to_string(), encoded);
        })+};
    }

    #[test]
    fn ids_round_trip_through_canonical_ulid_text() {
        assert_id_roundtrip!(
            AgentId,
            RoomId,
            ConversationId,
            MessageId,
            WorkItemId,
            ResultId,
            PublishId,
            SessionBindingId,
            CheckpointId,
            MemoryId,
        );
    }

    #[test]
    fn id_rejects_invalid_ulid_text() {
        assert!(AgentId::from_str("not-a-ulid").is_err());
    }
}
