use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ReminderError {
    #[error("reminder id is invalid")]
    InvalidId,
    #[error("reminder channel is unsupported")]
    InvalidChannel,
    #[error("reminder lifecycle transition is invalid")]
    InvalidLifecycle,
    #[error("reminder timestamp history is invalid")]
    InvalidTimestamps,
    #[error("reminder version is invalid")]
    InvalidVersion,
    #[error("delivery idempotency key must not be empty")]
    EmptyIdempotencyKey,
    #[error("delivery state is invalid")]
    InvalidDeliveryState,
}

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);
        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }
            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }
        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}
id_type!(ReminderId);
id_type!(DeliveryId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Channel {
    Tui,
    Desktop,
    Webhook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReminderLifecycle {
    Active,
    Paused,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeliveryStatus {
    Pending,
    Sent,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reminder {
    pub id: ReminderId,
    pub todo_id: crate::domain::TodoId,
    pub remind_at: DateTime<Utc>,
    pub channel: Channel,
    pub lifecycle: ReminderLifecycle,
    pub version: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
impl Reminder {
    pub fn validate(&self) -> Result<(), ReminderError> {
        if self.version == 0 || self.version > i64::MAX as u64 {
            return Err(ReminderError::InvalidVersion);
        }
        if self.updated_at < self.created_at {
            return Err(ReminderError::InvalidTimestamps);
        }
        Ok(())
    }
    pub fn transition(&mut self, next: ReminderLifecycle) -> Result<(), ReminderError> {
        self.transition_at(next, Utc::now())
    }

    pub fn transition_at(
        &mut self,
        next: ReminderLifecycle,
        updated_at: DateTime<Utc>,
    ) -> Result<(), ReminderError> {
        self.validate()?;
        match (self.lifecycle, next) {
            (
                ReminderLifecycle::Active,
                ReminderLifecycle::Paused | ReminderLifecycle::Cancelled,
            )
            | (
                ReminderLifecycle::Paused,
                ReminderLifecycle::Active | ReminderLifecycle::Cancelled,
            )
            | (ReminderLifecycle::Cancelled, ReminderLifecycle::Active) => {
                let version = self
                    .version
                    .checked_add(1)
                    .ok_or(ReminderError::InvalidVersion)?;
                if version > i64::MAX as u64 {
                    return Err(ReminderError::InvalidVersion);
                }
                if updated_at < self.updated_at {
                    return Err(ReminderError::InvalidTimestamps);
                }
                self.lifecycle = next;
                self.version = version;
                self.updated_at = updated_at;
                Ok(())
            }
            (current, requested) if current == requested => Ok(()),
            _ => Err(ReminderError::InvalidLifecycle),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRecord {
    pub id: DeliveryId,
    pub reminder_id: ReminderId,
    pub idempotency_key: String,
    pub status: DeliveryStatus,
    pub attempted_at: Option<DateTime<Utc>>,
    pub provider_reference: Option<String>,
    pub failure_code: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize)]
struct ReminderWire {
    id: ReminderId,
    todo_id: crate::domain::TodoId,
    remind_at: DateTime<Utc>,
    channel: Channel,
    lifecycle: ReminderLifecycle,
    version: u64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}
impl<'de> Deserialize<'de> for Reminder {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ReminderWire::deserialize(deserializer)?;
        let reminder = Self {
            id: wire.id,
            todo_id: wire.todo_id,
            remind_at: wire.remind_at,
            channel: wire.channel,
            lifecycle: wire.lifecycle,
            version: wire.version,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
        };
        reminder.validate().map_err(serde::de::Error::custom)?;
        Ok(reminder)
    }
}

impl Serialize for Reminder {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(serde::ser::Error::custom)?;
        ReminderWire {
            id: self.id,
            todo_id: self.todo_id,
            remind_at: self.remind_at,
            channel: self.channel,
            lifecycle: self.lifecycle,
            version: self.version,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
        .serialize(serializer)
    }
}

#[derive(Serialize, Deserialize)]
struct DeliveryWire {
    id: DeliveryId,
    reminder_id: ReminderId,
    idempotency_key: String,
    status: DeliveryStatus,
    attempted_at: Option<DateTime<Utc>>,
    provider_reference: Option<String>,
    failure_code: Option<String>,
    created_at: DateTime<Utc>,
}
impl<'de> Deserialize<'de> for DeliveryRecord {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = DeliveryWire::deserialize(deserializer)?;
        let record = Self {
            id: wire.id,
            reminder_id: wire.reminder_id,
            idempotency_key: wire.idempotency_key,
            status: wire.status,
            attempted_at: wire.attempted_at,
            provider_reference: wire.provider_reference,
            failure_code: wire.failure_code,
            created_at: wire.created_at,
        };
        record.validate().map_err(serde::de::Error::custom)?;
        Ok(record)
    }
}

impl Serialize for DeliveryRecord {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.validate().map_err(serde::ser::Error::custom)?;
        DeliveryWire {
            id: self.id,
            reminder_id: self.reminder_id,
            idempotency_key: self.idempotency_key.clone(),
            status: self.status,
            attempted_at: self.attempted_at,
            provider_reference: self.provider_reference.clone(),
            failure_code: self.failure_code.clone(),
            created_at: self.created_at,
        }
        .serialize(serializer)
    }
}

impl DeliveryRecord {
    pub fn validate(&self) -> Result<(), ReminderError> {
        if self.idempotency_key.trim().is_empty() {
            return Err(ReminderError::EmptyIdempotencyKey);
        }
        if self.attempted_at.is_some_and(|at| at < self.created_at) {
            return Err(ReminderError::InvalidTimestamps);
        }
        match self.status {
            DeliveryStatus::Pending if self.attempted_at.is_none() => Ok(()),
            DeliveryStatus::Sent
                if self.attempted_at.is_some() && self.provider_reference.is_some() =>
            {
                Ok(())
            }
            DeliveryStatus::Failed
                if self.attempted_at.is_some() && self.failure_code.is_some() =>
            {
                Ok(())
            }
            _ => Err(ReminderError::InvalidDeliveryState),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{TodoId, Version};
    fn reminder() -> Reminder {
        let now = Utc::now();
        Reminder {
            id: ReminderId::new(),
            todo_id: TodoId::new(),
            remind_at: now,
            channel: Channel::Tui,
            lifecycle: ReminderLifecycle::Active,
            version: Version::new().value(),
            created_at: now,
            updated_at: now,
        }
    }
    #[test]
    fn lifecycle_transition_increments_version() {
        let mut value = reminder();
        value.transition(ReminderLifecycle::Paused).unwrap();
        assert_eq!(value.version, 2);
    }
    #[test]
    fn delivery_requires_state_appropriate_fields() {
        let now = Utc::now();
        let value = DeliveryRecord {
            id: DeliveryId::new(),
            reminder_id: ReminderId::new(),
            idempotency_key: "r-1".into(),
            status: DeliveryStatus::Sent,
            attempted_at: Some(now),
            provider_reference: None,
            failure_code: None,
            created_at: now,
        };
        assert_eq!(value.validate(), Err(ReminderError::InvalidDeliveryState));
    }

    #[test]
    fn overflowed_transition_does_not_mutate() {
        let mut value = reminder();
        value.version = u64::MAX;
        let before = value.clone();
        assert_eq!(
            value.transition(ReminderLifecycle::Paused),
            Err(ReminderError::InvalidVersion)
        );
        assert_eq!(value, before);
    }

    #[test]
    fn version_is_bounded_for_postgres_and_transition_updates_timestamp() {
        let mut value = reminder();
        value.version = i64::MAX as u64;
        assert_eq!(value.validate(), Ok(()));
        let before = value.clone();
        assert_eq!(
            value.transition_at(ReminderLifecycle::Paused, before.updated_at),
            Err(ReminderError::InvalidVersion)
        );
        assert_eq!(value, before);

        let updated = before.updated_at + chrono::Duration::seconds(1);
        value.version = 1;
        value
            .transition_at(ReminderLifecycle::Paused, updated)
            .unwrap();
        assert_eq!(value.updated_at, updated);
    }
}
