use std::time::{Duration, Instant};

use iced::widget::{Space, button, column, container, row, text};
use iced::{Alignment, Color, Element, Length, Task, theme};

// SVG Icons for call controls
const PHONE_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="M20.01 15.38c-1.23 0-2.42-.2-3.53-.56a.977.977 0 0 0-1.01.24l-1.57 1.97c-2.83-1.35-5.48-3.9-6.89-6.83l1.95-1.66c.27-.28.35-.67.24-1.02-.37-1.11-.56-2.3-.56-3.53 0-.54-.45-.99-.99-.99H4.19C3.65 3 3 3.24 3 3.99 3 13.28 10.73 21 20.01 21c.71 0 .99-.63.99-1.18v-3.45c0-.54-.45-.99-.99-.99z"/>
</svg>"#;

const HANGUP_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="M12 9c-1.6 0-3.15.25-4.6.72v3.1c0 .39-.23.74-.56.9-.98.49-1.87 1.12-2.66 1.85-.18.18-.43.28-.68.28-.3 0-.55-.13-.74-.33l-2.31-2.31a.96.96 0 0 1-.29-.7c0-.28.11-.54.29-.71C3.85 8.09 7.75 6 12 6s8.15 2.09 11.55 5.8c.18.17.29.43.29.71 0 .28-.11.54-.29.7l-2.31 2.31c-.19.2-.44.33-.74.33-.25 0-.5-.1-.68-.28a9.27 9.27 0 0 0-2.66-1.85.978.978 0 0 1-.56-.9v-3.1C15.15 9.25 13.6 9 12 9z"/>
</svg>"#;

const MIC_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="M12 14c1.66 0 2.99-1.34 2.99-3L15 5c0-1.66-1.34-3-3-3S9 3.34 9 5v6c0 1.66 1.34 3 3 3zm5.3-3c0 3-2.54 5.1-5.3 5.1S6.7 14 6.7 11H5c0 3.41 2.72 6.23 6 6.72V21h2v-3.28c3.28-.48 6-3.3 6-6.72h-1.7z"/>
</svg>"#;

const MIC_OFF_ICON: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="currentColor">
    <path d="M19 11h-1.7c0 .74-.16 1.43-.43 2.05l1.23 1.23c.56-.98.9-2.09.9-3.28zm-4.02.17c0-.06.02-.11.02-.17V5c0-1.66-1.34-3-3-3S9 3.34 9 5v.18l5.98 5.99zM4.27 3L3 4.27l6.01 6.01V11c0 1.66 1.33 3 2.99 3 .22 0 .44-.03.65-.08l1.66 1.66c-.71.33-1.5.52-2.31.52-2.76 0-5.3-2.1-5.3-5.1H5c0 3.41 2.72 6.23 6 6.72V21h2v-3.28c.91-.13 1.77-.45 2.54-.9L19.73 21 21 19.73 4.27 3z"/>
</svg>"#;

#[derive(Clone, Debug)]
pub enum CallMessage {
    AcceptCall,
    RejectCall,
    HangupCall,
    ToggleMute,

    UpdateCallDuration,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CallState {
    Idle,
    IncomingCall,
    OutgoingCall,
    Connecting,
    Connected,
    Ended,
}

pub struct CallScreen {
    state: CallState,
    peer_address: String,
    peer_name: String,
    is_muted: bool,
    call_start_time: Option<Instant>,
    call_duration: Duration,
    error_message: Option<String>,
}

impl CallScreen {
    pub fn new() -> Self {
        Self {
            state: CallState::Idle,
            peer_address: String::new(),
            peer_name: String::new(),
            is_muted: false,
            call_start_time: None,
            call_duration: Duration::ZERO,
            error_message: None,
        }
    }

    pub fn start_outgoing_call(&mut self, address: String, name: String) {
        self.state = CallState::OutgoingCall;
        self.peer_address = address;
        self.peer_name = name;
        self.call_start_time = None;
        self.call_duration = Duration::ZERO;
        self.error_message = None;
    }

    pub fn incoming_call(&mut self, address: String, name: String) {
        self.state = CallState::IncomingCall;
        self.peer_address = address;
        self.peer_name = name;
        self.call_start_time = None;
        self.call_duration = Duration::ZERO;
        self.error_message = None;
    }

    pub fn call_connected(&mut self) {
        self.state = CallState::Connected;
        self.call_start_time = Some(Instant::now());
    }

    pub fn call_ended(&mut self, reason: Option<String>) {
        self.state = CallState::Ended;
        self.error_message = reason;
    }

    pub fn update_duration(&mut self) {
        if let Some(start_time) = self.call_start_time {
            self.call_duration = Instant::now() - start_time;
        }
    }

    pub fn is_active(&self) -> bool {
        !matches!(self.state, CallState::Idle | CallState::Ended)
    }

    pub fn view(&self) -> Element<'_, CallMessage> {
        let content = match &self.state {
            CallState::Idle => container(text("No active call").size(20))
                .center_x(Length::Fill)
                .center_y(Length::Fill),
            CallState::IncomingCall => {
                let call_type = "Voice Call";

                container(
                    column![
                        text("Incoming Call").size(24),
                        Space::with_height(10),
                        text(&self.peer_name).size(32),
                        text(&self.peer_address)
                            .size(14)
                            .color(Color::from_rgb(0.5, 0.5, 0.5)),
                        Space::with_height(10),
                        text(call_type).size(18),
                        Space::with_height(30),
                        row![
                            button(
                                container(
                                    create_svg(svg::Handle::from_memory(PHONE_ICON.as_bytes()))
                                        .width(32)
                                        .height(32)
                                )
                                .padding(10)
                            )
                            .on_press(CallMessage::AcceptCall)
                            .style(button::primary),
                            Space::with_width(20),
                            button(
                                container(
                                    create_svg(svg::Handle::from_memory(HANGUP_ICON.as_bytes()))
                                        .width(32)
                                        .height(32)
                                )
                                .padding(10)
                            )
                            .on_press(CallMessage::RejectCall)
                            .style(button::danger),
                        ]
                        .align_y(Alignment::Center),
                    ]
                    .align_x(Alignment::Center)
                    .spacing(10),
                )
                .center_x(Length::Fill)
                .center_y(Length::Fill)
            }
            CallState::OutgoingCall => container(
                column![
                    text("Calling...").size(24),
                    Space::with_height(10),
                    text(&self.peer_name).size(32),
                    text(&self.peer_address)
                        .size(14)
                        .color(Color::from_rgb(0.5, 0.5, 0.5)),
                    Space::with_height(30),
                    button(
                        container(
                            create_svg(svg::Handle::from_memory(HANGUP_ICON.as_bytes()))
                                .width(32)
                                .height(32)
                        )
                        .padding(10)
                    )
                    .on_press(CallMessage::HangupCall)
                    .style(button::danger),
                ]
                .align_x(Alignment::Center)
                .spacing(10),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill),
            CallState::Connecting => container(
                column![
                    text("Connecting...").size(24),
                    Space::with_height(10),
                    text(&self.peer_name).size(32),
                    text(&self.peer_address)
                        .size(14)
                        .color(Color::from_rgb(0.5, 0.5, 0.5)),
                ]
                .align_x(Alignment::Center)
                .spacing(10),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill),
            CallState::Connected => {
                let duration_text = format_duration(self.call_duration);

                let mut controls = vec![
                    button(
                        container(
                            create_svg(svg::Handle::from_memory(if self.is_muted {
                                MIC_OFF_ICON.as_bytes()
                            } else {
                                MIC_ICON.as_bytes()
                            }))
                            .width(32)
                            .height(32),
                        )
                        .padding(10),
                    )
                    .on_press(CallMessage::ToggleMute)
                    .style(if self.is_muted {
                        button::secondary
                    } else {
                        button::primary
                    })
                    .into(),
                ];

                controls.push(Space::with_width(20).into());
                controls.push(
                    button(
                        container(
                            create_svg(svg::Handle::from_memory(HANGUP_ICON.as_bytes()))
                                .width(32)
                                .height(32),
                        )
                        .padding(10),
                    )
                    .on_press(CallMessage::HangupCall)
                    .style(button::danger)
                    .into(),
                );

                // Voice call layout
                let main_content = column![
                    container(
                        column![
                            text(&self.peer_name).size(32),
                            text(&self.peer_address)
                                .size(14)
                                .color(Color::from_rgb(0.5, 0.5, 0.5)),
                            Space::with_height(20),
                            text(duration_text).size(24),
                            Space::with_height(30),
                            row(controls).align_y(Alignment::Center),
                        ]
                        .align_x(Alignment::Center)
                        .spacing(10)
                    )
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding(40)
                    .style(container::rounded_box),
                ];

                container(main_content)
                    .width(Length::Fill)
                    .height(Length::Fill)
            }
            CallState::Ended => {
                let message = if let Some(ref error) = self.error_message {
                    format!("Call ended: {}", error)
                } else {
                    "Call ended".to_string()
                };

                container(
                    column![
                        text(message).size(20),
                        Space::with_height(10),
                        text(format!("Duration: {}", format_duration(self.call_duration))).size(16),
                    ]
                    .align_x(Alignment::Center)
                    .spacing(10),
                )
                .center_x(Length::Fill)
                .center_y(Length::Fill)
            }
        };

        content.into()
    }

    pub fn update(&mut self, message: CallMessage) -> Task<CallMessage> {
        match message {
            CallMessage::AcceptCall => {
                if self.state == CallState::IncomingCall {
                    self.state = CallState::Connecting;
                }
            }
            CallMessage::RejectCall => {
                if self.state == CallState::IncomingCall {
                    self.call_ended(Some("Call rejected".to_string()));
                }
            }
            CallMessage::HangupCall => {
                if self.is_active() {
                    self.call_ended(Some("Call ended by user".to_string()));
                }
            }
            CallMessage::ToggleMute => {
                self.is_muted = !self.is_muted;
            }
            CallMessage::UpdateCallDuration => {
                self.update_duration();
            }
        }
        Task::none()
    }
}

fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.as_secs();
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{:02}:{:02}", minutes, seconds)
    }
}

// Helper function for SVG rendering
use iced::widget::svg;

fn create_svg(handle: svg::Handle) -> svg::Svg<'static, theme::Theme> {
    svg::Svg::new(handle)
}
