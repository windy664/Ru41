pub mod cli;
pub mod config;
pub mod qq_command;
pub mod service;
pub mod source;

pub use config::{TeachermateConfig, TeachermateSection};
// TODO(同学重构中): TeachermateCli/TeachermateCommand 尚未在 cli.rs 实现，先不导出
pub use qq_command::TeachermateQqCommand;
pub use service::{
    TeachermatePollReport, TeachermateTracker, TeachermateWorkPollReport, TeachermateWorkTracker,
};
pub use source::{
    ActiveSign, CourseHomework, Homework, HomeworkCourse, SignsOutcome, StudentRole,
    TeachermateClient, homework_status_label, homework_status_open, sign_response_ok,
};
