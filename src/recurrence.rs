use chrono::{Months, NaiveDate};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RecurrenceError {
    #[error("recurrence interval must be between 1 and 366")]
    InvalidInterval,
    #[error("recurrence count must be between 1 and 1000")]
    InvalidCount,
    #[error("recurrence requires count or until")]
    MissingBound,
    #[error("recurrence until must be after the start date")]
    UntilNotAfterStart,
    #[error("recurrence range is invalid")]
    InvalidRange,
    #[error("recurrence expansion overflowed the date range")]
    ExpansionOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub frequency: Frequency,
    pub interval: u32,
    pub count: Option<u32>,
    pub until: Option<NaiveDate>,
}

impl Rule {
    pub fn new(
        frequency: Frequency,
        interval: u32,
        count: Option<u32>,
        until: Option<NaiveDate>,
        start: NaiveDate,
    ) -> Result<Self, RecurrenceError> {
        let rule = Self {
            frequency,
            interval,
            count,
            until,
        };
        rule.validate(start)?;
        Ok(rule)
    }
    pub fn validate(&self, start: NaiveDate) -> Result<(), RecurrenceError> {
        if !(1..=366).contains(&self.interval) {
            return Err(RecurrenceError::InvalidInterval);
        }
        if self.count.is_some_and(|count| !(1..=1000).contains(&count)) {
            return Err(RecurrenceError::InvalidCount);
        }
        if self.count.is_none() && self.until.is_none() {
            return Err(RecurrenceError::MissingBound);
        }
        if self.until.is_some_and(|until| until <= start) {
            return Err(RecurrenceError::UntilNotAfterStart);
        }
        Ok(())
    }
    pub fn expand(
        &self,
        start: NaiveDate,
        from: NaiveDate,
        through: NaiveDate,
    ) -> Result<Vec<NaiveDate>, RecurrenceError> {
        if from > through {
            return Err(RecurrenceError::InvalidRange);
        }
        self.validate(start)?;
        let mut result = Vec::new();
        let mut current = start;
        for occurrence in 0..=100_000_u32 {
            if current > through || self.until.is_some_and(|until| current > until) {
                return Ok(result);
            }
            if current >= from {
                result.push(current);
            }
            if self.count.is_some_and(|count| occurrence + 1 >= count) {
                return Ok(result);
            }
            if current >= through || self.until.is_some_and(|until| current >= until) {
                return Ok(result);
            }
            current = match self.frequency {
                Frequency::Daily => {
                    current.checked_add_days(chrono::Days::new(u64::from(self.interval)))
                }
                Frequency::Weekly => {
                    current.checked_add_days(chrono::Days::new(u64::from(self.interval) * 7))
                }
                Frequency::Monthly => current.checked_add_months(Months::new(self.interval)),
            }
            .ok_or(RecurrenceError::ExpansionOverflow)?;
        }
        Err(RecurrenceError::ExpansionOverflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_monthly_expansion_is_deterministic() {
        let start = NaiveDate::from_ymd_opt(2026, 1, 31).unwrap();
        let rule = Rule::new(Frequency::Monthly, 1, Some(3), None, start).unwrap();
        assert_eq!(
            rule.expand(start, start, NaiveDate::from_ymd_opt(2026, 5, 1).unwrap())
                .unwrap(),
            vec![
                start,
                NaiveDate::from_ymd_opt(2026, 2, 28).unwrap(),
                NaiveDate::from_ymd_opt(2026, 3, 28).unwrap()
            ]
        );
    }
    #[test]
    fn unbounded_rules_are_rejected() {
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        assert_eq!(
            Rule::new(Frequency::Daily, 1, None, None, start),
            Err(RecurrenceError::MissingBound)
        );
    }

    #[test]
    fn distant_until_bounds_are_not_silently_truncated() {
        let start = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let until = NaiveDate::from_ymd_opt(2030, 1, 1).unwrap();
        let rule = Rule::new(Frequency::Daily, 1, None, Some(until), start).unwrap();
        let dates = rule.expand(start, start, until).unwrap();
        assert_eq!(dates.len(), 1462);
        assert_eq!(dates.last(), Some(&until));
    }

    #[test]
    fn maximum_until_date_is_emitted_without_post_bound_overflow() {
        let start = NaiveDate::MAX.pred_opt().unwrap();
        let rule = Rule::new(Frequency::Daily, 1, None, Some(NaiveDate::MAX), start).unwrap();
        assert_eq!(
            rule.expand(start, start, NaiveDate::MAX).unwrap(),
            vec![start, NaiveDate::MAX]
        );
    }
}
