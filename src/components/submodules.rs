use super::{
	utils::scroll_vertical::VerticalScroll, visibility_blocking,
	CommandBlocking, CommandInfo, Component, DrawableComponent,
	EventState, ScrollType,
};
use crate::{
	keys::{key_match, SharedKeyConfig},
	strings,
	ui::{self, Size},
};
use anyhow::Result;
use asyncgit::sync::{get_submodules, RepoPathRef, SubmoduleInfo};
use crossterm::event::Event;
use std::{cell::Cell, convert::TryInto};
use tui::{
	backend::Backend,
	layout::{
		Alignment, Constraint, Direction, Layout, Margin, Rect,
	},
	text::{Span, Spans, Text},
	widgets::{Block, BorderType, Borders, Clear, Paragraph},
	Frame,
};
use ui::style::SharedTheme;
use unicode_truncate::UnicodeTruncateStr;

///
pub struct SubmodulesListComponent {
	repo: RepoPathRef,
	submodules: Vec<SubmoduleInfo>,
	visible: bool,
	current_height: Cell<u16>,
	selection: u16,
	scroll: VerticalScroll,
	theme: SharedTheme,
	key_config: SharedKeyConfig,
}

impl DrawableComponent for SubmodulesListComponent {
	fn draw<B: Backend>(
		&self,
		f: &mut Frame<B>,
		rect: Rect,
	) -> Result<()> {
		if self.is_visible() {
			const PERCENT_SIZE: Size = Size::new(80, 80);
			const MIN_SIZE: Size = Size::new(60, 30);

			let area = ui::centered_rect(
				PERCENT_SIZE.width,
				PERCENT_SIZE.height,
				rect,
			);
			let area = ui::rect_inside(MIN_SIZE, rect.into(), area);
			let area = area.intersection(rect);

			f.render_widget(Clear, area);

			f.render_widget(
				Block::default()
					.title(strings::POPUP_TITLE_SUBMODULES)
					.border_type(BorderType::Thick)
					.borders(Borders::ALL),
				area,
			);

			let area = area.inner(&Margin {
				vertical: 1,
				horizontal: 1,
			});

			let chunks = Layout::default()
				.direction(Direction::Horizontal)
				.constraints(
					[Constraint::Min(40), Constraint::Length(40)]
						.as_ref(),
				)
				.split(area);

			self.draw_list(f, chunks[0])?;
			self.draw_info(f, chunks[1]);
		}

		Ok(())
	}
}

impl Component for SubmodulesListComponent {
	fn commands(
		&self,
		out: &mut Vec<CommandInfo>,
		force_all: bool,
	) -> CommandBlocking {
		if self.visible || force_all {
			if !force_all {
				out.clear();
			}

			out.push(CommandInfo::new(
				strings::commands::scroll(&self.key_config),
				true,
				true,
			));

			out.push(CommandInfo::new(
				strings::commands::close_popup(&self.key_config),
				true,
				true,
			));
		}
		visibility_blocking(self)
	}

	fn event(&mut self, ev: &Event) -> Result<EventState> {
		if !self.visible {
			return Ok(EventState::NotConsumed);
		}

		if let Event::Key(e) = ev {
			if key_match(e, self.key_config.keys.exit_popup) {
				self.hide();
			} else if key_match(e, self.key_config.keys.move_down) {
				return self
					.move_selection(ScrollType::Up)
					.map(Into::into);
			} else if key_match(e, self.key_config.keys.move_up) {
				return self
					.move_selection(ScrollType::Down)
					.map(Into::into);
			} else if key_match(e, self.key_config.keys.page_down) {
				return self
					.move_selection(ScrollType::PageDown)
					.map(Into::into);
			} else if key_match(e, self.key_config.keys.page_up) {
				return self
					.move_selection(ScrollType::PageUp)
					.map(Into::into);
			} else if key_match(e, self.key_config.keys.home) {
				return self
					.move_selection(ScrollType::Home)
					.map(Into::into);
			} else if key_match(e, self.key_config.keys.end) {
				return self
					.move_selection(ScrollType::End)
					.map(Into::into);
			} else if key_match(
				e,
				self.key_config.keys.cmd_bar_toggle,
			) {
				//do not consume if its the more key
				return Ok(EventState::NotConsumed);
			}
		}

		Ok(EventState::Consumed)
	}

	fn is_visible(&self) -> bool {
		self.visible
	}

	fn hide(&mut self) {
		self.visible = false;
	}

	fn show(&mut self) -> Result<()> {
		self.visible = true;

		Ok(())
	}
}

impl SubmodulesListComponent {
	pub fn new(
		repo: RepoPathRef,
		theme: SharedTheme,
		key_config: SharedKeyConfig,
	) -> Self {
		Self {
			submodules: Vec::new(),
			scroll: VerticalScroll::new(),
			selection: 0,
			visible: false,
			theme,
			key_config,
			current_height: Cell::new(0),
			repo,
		}
	}

	///
	pub fn open(&mut self) -> Result<()> {
		self.show()?;
		self.update_submodules()?;

		Ok(())
	}

	///
	pub fn update_submodules(&mut self) -> Result<()> {
		if self.is_visible() {
			self.submodules = get_submodules(&self.repo.borrow())?;

			self.set_selection(self.selection)?;
		}
		Ok(())
	}

	fn selected_entry(&self) -> Option<&SubmoduleInfo> {
		self.submodules.get(self.selection as usize)
	}

	//TODO: dedup this almost identical with BranchListComponent
	fn move_selection(&mut self, scroll: ScrollType) -> Result<bool> {
		let new_selection = match scroll {
			ScrollType::Up => self.selection.saturating_add(1),
			ScrollType::Down => self.selection.saturating_sub(1),
			ScrollType::PageDown => self
				.selection
				.saturating_add(self.current_height.get()),
			ScrollType::PageUp => self
				.selection
				.saturating_sub(self.current_height.get()),
			ScrollType::Home => 0,
			ScrollType::End => {
				let count: u16 = self.submodules.len().try_into()?;
				count.saturating_sub(1)
			}
		};

		self.set_selection(new_selection)?;

		Ok(true)
	}

	fn set_selection(&mut self, selection: u16) -> Result<()> {
		let num_branches: u16 = self.submodules.len().try_into()?;
		let num_branches = num_branches.saturating_sub(1);

		let selection = if selection > num_branches {
			num_branches
		} else {
			selection
		};

		self.selection = selection;

		Ok(())
	}

	fn get_text(
		&self,
		theme: &SharedTheme,
		width_available: u16,
		height: usize,
	) -> Text {
		const THREE_DOTS: &str = "...";
		const THREE_DOTS_LENGTH: usize = THREE_DOTS.len(); // "..."
		const COMMIT_HASH_LENGTH: usize = 8;

		let mut txt = Vec::with_capacity(3);

		let name_length: usize = (width_available as usize)
			.saturating_sub(COMMIT_HASH_LENGTH)
			.saturating_sub(THREE_DOTS_LENGTH);

		for (i, submodule) in self
			.submodules
			.iter()
			.skip(self.scroll.get_top())
			.take(height)
			.enumerate()
		{
			let mut module_path = submodule
				.path
				.as_os_str()
				.to_string_lossy()
				.to_string();

			if module_path.len() > name_length {
				module_path.unicode_truncate(
					name_length.saturating_sub(THREE_DOTS_LENGTH),
				);
				module_path += THREE_DOTS;
			}

			let selected = (self.selection as usize
				- self.scroll.get_top())
				== i;

			let span_hash = Span::styled(
				format!(
					"{} ",
					submodule
						.head_id
						.unwrap_or_default()
						.get_short_string()
				),
				theme.commit_hash(selected),
			);

			let span_name = Span::styled(
				format!("{:w$} ", module_path, w = name_length),
				theme.text(true, selected),
			);

			txt.push(Spans::from(vec![span_name, span_hash]));
		}

		Text::from(txt)
	}

	fn get_info_text(&self, theme: &SharedTheme) -> Text {
		self.selected_entry().map_or_else(
			Text::default,
			|submodule| {
				let span_title_path =
					Span::styled("Path:", theme.text(false, false));
				let span_path = Span::styled(
					submodule.path.to_string_lossy(),
					theme.text(true, false),
				);

				let span_title_commit =
					Span::styled("Commit:", theme.text(false, false));
				let span_commit = Span::styled(
					submodule.id.unwrap_or_default().to_string(),
					theme.commit_hash(false),
				);

				let span_title_url =
					Span::styled("Url:", theme.text(false, false));
				let span_url = Span::styled(
					submodule.url.clone().unwrap_or_default(),
					theme.text(true, false),
				);

				let span_title_status =
					Span::styled("Status:", theme.text(false, false));
				let span_status = Span::styled(
					format!("{:?}", submodule.status),
					theme.text(true, false),
				);

				Text::from(vec![
					Spans::from(vec![span_title_path]),
					Spans::from(vec![span_path]),
					Spans::from(vec![]),
					Spans::from(vec![span_title_commit]),
					Spans::from(vec![span_commit]),
					Spans::from(vec![]),
					Spans::from(vec![span_title_url]),
					Spans::from(vec![span_url]),
					Spans::from(vec![]),
					Spans::from(vec![span_title_status]),
					Spans::from(vec![span_status]),
				])
			},
		)
	}

	fn draw_list<B: Backend>(
		&self,
		f: &mut Frame<B>,
		r: Rect,
	) -> Result<()> {
		let height_in_lines = r.height as usize;
		self.current_height.set(height_in_lines.try_into()?);

		self.scroll.update(
			self.selection as usize,
			self.submodules.len(),
			height_in_lines,
		);

		f.render_widget(
			Paragraph::new(self.get_text(
				&self.theme,
				r.width,
				height_in_lines,
			))
			.alignment(Alignment::Left),
			r,
		);

		let mut r = r;
		r.height += 2;
		r.y = r.y.saturating_sub(1);

		self.scroll.draw(f, r, &self.theme);

		Ok(())
	}

	fn draw_info<B: Backend>(&self, f: &mut Frame<B>, r: Rect) {
		f.render_widget(
			Paragraph::new(self.get_info_text(&self.theme))
				.alignment(Alignment::Left),
			r,
		);
	}
}
