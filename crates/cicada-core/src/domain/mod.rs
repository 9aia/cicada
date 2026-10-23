//! Domain entities and value objects — pure, no I/O.

pub mod attendee;
pub mod calendar;
pub mod capabilities;
pub mod error;
pub mod event;
pub mod ids;
pub mod operation;
pub mod policy;
pub mod recurrence;
pub mod reminder;
pub mod state;
pub mod time;

pub use attendee::{Attendee, ResponseStatus};
pub use calendar::Calendar;
pub use capabilities::{ProviderCapabilities, RecurrenceCapabilities};
pub use error::{CicadaError, ErrorKind, Result};
pub use event::{Conferencing, Event, EventBody, EventTime, Transparency};
pub use ids::{ContentHash, LocalId, RemoteId, ZoneId};
pub use operation::{EventDiff, EventField, FieldChange, OpKind, Operation};
pub use policy::{SendUpdates, WriteOptions};
pub use recurrence::{
    ByDay, Frequency, Occurrence, RecurrenceExpander, RecurrenceRule, StructuredRule, Until,
    Weekday,
};
pub use reminder::{Reminder, ReminderKind};
pub use state::{JournalEntry, JournalOutcome, RecordedCalendar, RecordedEvent, RecordedState};
pub use time::{Time, TimeRange};
