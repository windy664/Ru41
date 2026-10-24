//! 微助教协议层：模型与 HTTP 客户端。

pub mod client;
pub mod model;

pub use client::{DEFAULT_API_BASE, EXPIRED_MARKER, TeachermateClient, sign_response_ok};
pub use model::{
    ActiveSign, CourseHomework, Homework, HomeworkCourse, SignsOutcome, StudentRole,
    homework_status_label, homework_status_open, parse_course_homework, parse_homework_courses,
};
