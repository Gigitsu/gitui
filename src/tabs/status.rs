use crate::{
	accessors,
	components::{
		command_pump, event_pump, visibility_blocking,
		ChangesComponent, CommandBlocking, CommandInfo, Component,
		DiffComponent, DrawableComponent, EventState,
		FileTreeItemKind, SharedOptions,
	},
	keys::{key_match, SharedKeyConfig},
	queue::{Action, InternalEvent, NeedsUpdate, Queue, ResetItem},
	strings, try_or_popup,
	ui::style::SharedTheme,
};
use anyhow::Result;
use asyncgit::{
	cached,
	sync::{
		self, status::StatusType, RepoPath, RepoPathRef, RepoState,
	},
	sync::{BranchCompare, CommitId},
	AsyncDiff, AsyncGitNotification, AsyncStatus, DiffParams,
	DiffType, PushType, StatusParams,
};
use crossbeam_channel::Sender;
use crossterm::event::Event;
use itertools::Itertools;
use std::convert::Into;
use tui::{
	layout::{Alignment, Constraint, Direction, Layout},
	style::{Color, Style},
	widgets::{Block, BorderType, Borders, Paragraph},
};

/// what part of the screen is focused
#[derive(PartialEq)]
enum Focus {
	WorkDir,
	Diff,
	Stage,
}

/// focus can toggle between workdir and stage
impl Focus {
	const fn toggled_focus(&self) -> Self {
		match self {
			Self::WorkDir => Self::Stage,
			Self::Stage => Self::WorkDir,
			Self::Diff => Self::Diff,
		}
	}
}

/// which target are we showing a diff against
#[derive(PartialEq, Copy, Clone)]
enum DiffTarget {
	Stage,
	WorkingDir,
}

pub struct Status {
	repo: RepoPathRef,
	visible: bool,
	focus: Focus,
	diff_target: DiffTarget,
	index: ChangesComponent,
	index_wd: ChangesComponent,
	diff: DiffComponent,
	git_diff: AsyncDiff,
	has_remotes: bool,
	git_state: RepoState,
	git_status_workdir: AsyncStatus,
	git_status_stage: AsyncStatus,
	git_branch_state: Option<BranchCompare>,
	git_branch_name: cached::BranchName,
	queue: Queue,
	git_action_executed: bool,
	options: SharedOptions,
	key_config: SharedKeyConfig,
}

impl DrawableComponent for Status {
	fn draw<B: tui::backend::Backend>(
		&self,
		f: &mut tui::Frame<B>,
		rect: tui::layout::Rect,
	) -> Result<()> {
		let repo_unclean = self.repo_state_unclean();
		let rects = if repo_unclean {
			Layout::default()
				.direction(Direction::Vertical)
				.constraints(
					[Constraint::Min(1), Constraint::Length(3)]
						.as_ref(),
				)
				.split(rect)
		} else {
			vec![rect]
		};

		let chunks = Layout::default()
			.direction(Direction::Horizontal)
			.constraints(
				if self.focus == Focus::Diff {
					[
						Constraint::Percentage(30),
						Constraint::Percentage(70),
					]
				} else {
					[
						Constraint::Percentage(50),
						Constraint::Percentage(50),
					]
				}
				.as_ref(),
			)
			.split(rects[0]);

		let left_chunks = Layout::default()
			.direction(Direction::Vertical)
			.constraints(
				if self.diff_target == DiffTarget::WorkingDir {
					[
						Constraint::Percentage(60),
						Constraint::Percentage(40),
					]
				} else {
					[
						Constraint::Percentage(40),
						Constraint::Percentage(60),
					]
				}
				.as_ref(),
			)
			.split(chunks[0]);

		self.index_wd.draw(f, left_chunks[0])?;
		self.index.draw(f, left_chunks[1])?;
		self.diff.draw(f, chunks[1])?;
		self.draw_branch_state(f, &left_chunks);

		if repo_unclean {
			self.draw_repo_state(f, rects[1]);
		}

		Ok(())
	}
}

impl Status {
	accessors!(self, [index, index_wd, diff]);

	///
	pub fn new(
		repo: RepoPathRef,
		queue: &Queue,
		sender: &Sender<AsyncGitNotification>,
		theme: SharedTheme,
		key_config: SharedKeyConfig,
		options: SharedOptions,
	) -> Self {
		let repo_clone = repo.borrow().clone();
		Self {
			queue: queue.clone(),
			visible: true,
			has_remotes: false,
			git_state: RepoState::Clean,
			focus: Focus::WorkDir,
			diff_target: DiffTarget::WorkingDir,
			index_wd: ChangesComponent::new(
				repo.clone(),
				&strings::title_status(&key_config),
				true,
				true,
				queue.clone(),
				theme.clone(),
				key_config.clone(),
				options.clone(),
			),
			index: ChangesComponent::new(
				repo.clone(),
				&strings::title_index(&key_config),
				false,
				false,
				queue.clone(),
				theme.clone(),
				key_config.clone(),
				options.clone(),
			),
			diff: DiffComponent::new(
				repo.clone(),
				queue.clone(),
				theme,
				key_config.clone(),
				false,
			),
			git_diff: AsyncDiff::new(repo_clone.clone(), sender),
			git_status_workdir: AsyncStatus::new(
				repo_clone.clone(),
				sender.clone(),
			),
			git_status_stage: AsyncStatus::new(
				repo_clone,
				sender.clone(),
			),
			git_action_executed: false,
			git_branch_state: None,
			git_branch_name: cached::BranchName::new(repo.clone()),
			key_config,
			options,
			repo,
		}
	}

	fn draw_branch_state<B: tui::backend::Backend>(
		&self,
		f: &mut tui::Frame<B>,
		chunks: &[tui::layout::Rect],
	) {
		if let Some(branch_name) = self.git_branch_name.last() {
			let ahead_behind = self
				.git_branch_state
				.as_ref()
				.map_or_else(String::new, |state| {
					format!(
						"\u{2191}{} \u{2193}{} ",
						state.ahead, state.behind,
					)
				});

			let w = Paragraph::new(format!(
				"{}{{{}}}",
				ahead_behind, branch_name
			))
			.alignment(Alignment::Right);

			let mut rect = if self.index_wd.focused() {
				let mut rect = chunks[0];
				rect.y += rect.height.saturating_sub(1);
				rect
			} else {
				chunks[1]
			};

			rect.x += 1;
			rect.width = rect.width.saturating_sub(2);
			rect.height = rect
				.height
				.saturating_sub(rect.height.saturating_sub(1));

			f.render_widget(w, rect);
		}
	}

	fn repo_state_text(repo: &RepoPath, state: &RepoState) -> String {
		match state {
			RepoState::Merge => {
				let ids =
					sync::mergehead_ids(repo).unwrap_or_default();

				format!(
					"Commits: {}",
					ids.iter()
						.map(sync::CommitId::get_short_string)
						.join(",")
				)
			}
			RepoState::Rebase => {
				if let Ok(p) = sync::rebase_progress(repo) {
					format!(
						"Step: {}/{} Current Commit: {}",
						p.current + 1,
						p.steps,
						p.current_commit
							.as_ref()
							.map(CommitId::get_short_string)
							.unwrap_or_default(),
					)
				} else {
					String::new()
				}
			}
			RepoState::Revert => {
				format!(
					"Revert {}",
					sync::revert_head(repo)
						.ok()
						.as_ref()
						.map(CommitId::get_short_string)
						.unwrap_or_default(),
				)
			}
			_ => format!("{:?}", state),
		}
	}

	fn draw_repo_state<B: tui::backend::Backend>(
		&self,
		f: &mut tui::Frame<B>,
		r: tui::layout::Rect,
	) {
		if self.git_state != RepoState::Clean {
			let txt = Self::repo_state_text(
				&self.repo.borrow(),
				&self.git_state,
			);

			let w = Paragraph::new(txt)
				.block(
					Block::default()
						.border_type(BorderType::Plain)
						.borders(Borders::all())
						.border_style(
							Style::default().fg(Color::Yellow),
						)
						.title(format!(
							"Pending {:?}",
							self.git_state
						)),
				)
				.style(Style::default().fg(Color::Red))
				.alignment(Alignment::Left);

			f.render_widget(w, r);
		}
	}

	fn repo_state_unclean(&self) -> bool {
		self.git_state != RepoState::Clean
	}

	fn can_focus_diff(&self) -> bool {
		match self.focus {
			Focus::WorkDir => self.index_wd.is_file_seleted(),
			Focus::Stage => self.index.is_file_seleted(),
			Focus::Diff => false,
		}
	}

	fn is_focus_on_diff(&self) -> bool {
		self.focus == Focus::Diff
	}

	fn switch_focus(&mut self, f: Focus) -> Result<bool> {
		if self.focus != f {
			self.focus = f;

			match self.focus {
				Focus::WorkDir => {
					self.set_diff_target(DiffTarget::WorkingDir);
					self.diff.focus(false);
				}
				Focus::Stage => {
					self.set_diff_target(DiffTarget::Stage);
					self.diff.focus(false);
				}
				Focus::Diff => {
					self.index.focus(false);
					self.index_wd.focus(false);

					self.diff.focus(true);
				}
			};

			self.update_diff()?;

			return Ok(true);
		}

		Ok(false)
	}

	fn set_diff_target(&mut self, target: DiffTarget) {
		self.diff_target = target;
		let is_stage = self.diff_target == DiffTarget::Stage;

		self.index_wd.focus_select(!is_stage);
		self.index.focus_select(is_stage);
	}

	pub fn selected_path(&self) -> Option<(String, bool)> {
		let (idx, is_stage) = match self.diff_target {
			DiffTarget::Stage => (&self.index, true),
			DiffTarget::WorkingDir => (&self.index_wd, false),
		};

		if let Some(item) = idx.selection() {
			if let FileTreeItemKind::File(i) = item.kind {
				return Some((i.path, is_stage));
			}
		}
		None
	}

	///
	pub fn update(&mut self) -> Result<()> {
		self.git_branch_name.lookup().map(Some).unwrap_or(None);

		if self.is_visible() {
			let config = self.options.borrow().status_show_untracked;

			self.git_diff.refresh()?;
			self.git_status_workdir.fetch(&StatusParams::new(
				StatusType::WorkingDir,
				config,
			))?;
			self.git_status_stage.fetch(&StatusParams::new(
				StatusType::Stage,
				config,
			))?;

			self.git_state = sync::repo_state(&self.repo.borrow())
				.unwrap_or(RepoState::Clean);

			self.branch_compare();
		}

		Ok(())
	}

	///
	pub fn anything_pending(&self) -> bool {
		self.git_diff.is_pending()
			|| self.git_status_stage.is_pending()
			|| self.git_status_workdir.is_pending()
	}

	fn check_remotes(&mut self) {
		self.has_remotes =
			sync::get_branches_info(&self.repo.borrow(), false)
				.map(|branches| !branches.is_empty())
				.unwrap_or(false);
	}

	///
	pub fn update_git(
		&mut self,
		ev: AsyncGitNotification,
	) -> Result<()> {
		match ev {
			AsyncGitNotification::Diff => self.update_diff()?,
			AsyncGitNotification::Status => self.update_status()?,
			AsyncGitNotification::Push
			| AsyncGitNotification::Pull
			| AsyncGitNotification::CommitFiles => {
				self.branch_compare();
			}
			_ => (),
		}

		Ok(())
	}

	fn update_status(&mut self) -> Result<()> {
		let stage_status = self.git_status_stage.last()?;
		self.index.set_items(&stage_status.items)?;

		let workdir_status = self.git_status_workdir.last()?;
		self.index_wd.set_items(&workdir_status.items)?;

		self.update_diff()?;
		self.check_remotes();

		if self.git_action_executed {
			self.git_action_executed = false;

			if self.focus == Focus::WorkDir
				&& workdir_status.items.is_empty()
				&& !stage_status.items.is_empty()
			{
				self.switch_focus(Focus::Stage)?;
			} else if self.focus == Focus::Stage
				&& stage_status.items.is_empty()
			{
				self.switch_focus(Focus::WorkDir)?;
			}
		}

		Ok(())
	}

	///
	pub fn update_diff(&mut self) -> Result<()> {
		if let Some((path, is_stage)) = self.selected_path() {
			let diff_type = if is_stage {
				DiffType::Stage
			} else {
				DiffType::WorkDir
			};

			let diff_params = DiffParams {
				path: path.clone(),
				diff_type,
				options: self.options.borrow().diff,
			};

			if self.diff.current() == (path.clone(), is_stage) {
				// we are already showing a diff of the right file
				// maybe the diff changed (outside file change)
				if let Some((params, last)) = self.git_diff.last()? {
					if params == diff_params {
						// all params match, so we might need to update
						self.diff.update(path, is_stage, last);
					} else {
						// params changed, we need to request the right diff
						self.request_diff(
							diff_params,
							path,
							is_stage,
						)?;
					}
				}
			} else {
				// we dont show the right diff right now, so we need to request
				self.request_diff(diff_params, path, is_stage)?;
			}
		} else {
			self.diff.clear(false);
		}

		Ok(())
	}

	fn request_diff(
		&mut self,
		diff_params: DiffParams,
		path: String,
		is_stage: bool,
	) -> Result<(), anyhow::Error> {
		if let Some(diff) = self.git_diff.request(diff_params)? {
			self.diff.update(path, is_stage, diff);
		} else {
			self.diff.clear(true);
		}

		Ok(())
	}

	/// called after confirmation
	pub fn reset(&mut self, item: &ResetItem) -> bool {
		if let Err(e) = sync::reset_workdir(
			&self.repo.borrow(),
			item.path.as_str(),
		) {
			self.queue.push(InternalEvent::ShowErrorMsg(format!(
				"reset failed:\n{}",
				e
			)));

			false
		} else {
			true
		}
	}

	pub fn last_file_moved(&mut self) -> Result<()> {
		if !self.is_focus_on_diff() && self.is_visible() {
			self.switch_focus(self.focus.toggled_focus())?;
		}
		Ok(())
	}

	fn push(&self, force: bool) {
		if self.can_push() {
			if let Some(branch) = self.git_branch_name.last() {
				if force {
					self.queue.push(InternalEvent::ConfirmAction(
						Action::ForcePush(branch, force),
					));
				} else {
					self.queue.push(InternalEvent::Push(
						branch,
						PushType::Branch,
						force,
						false,
					));
				}
			}
		}
	}

	fn pull(&self) {
		if let Some(branch) = self.git_branch_name.last() {
			self.queue.push(InternalEvent::Pull(branch));
		}
	}

	fn undo_last_commit(&self) {
		try_or_popup!(
			self,
			"undo commit failed:",
			sync::utils::undo_last_commit(&self.repo.borrow())
		);
	}

	fn branch_compare(&mut self) {
		self.git_branch_state =
			self.git_branch_name.last().and_then(|branch| {
				sync::branch_compare_upstream(
					&self.repo.borrow(),
					branch.as_str(),
				)
				.ok()
			});
	}

	fn can_push(&self) -> bool {
		self.git_branch_state
			.as_ref()
			.map_or(true, |state| state.ahead > 0)
			&& self.has_remotes
	}

	const fn can_pull(&self) -> bool {
		self.has_remotes && self.git_branch_state.is_some()
	}

	fn can_abort_merge(&self) -> bool {
		self.git_state == RepoState::Merge
	}

	fn pending_rebase(&self) -> bool {
		self.git_state == RepoState::Rebase
	}

	fn pending_revert(&self) -> bool {
		self.git_state == RepoState::Revert
	}

	pub fn revert_pending_state(&self) {
		try_or_popup!(
			self,
			"revert pending state",
			sync::abort_pending_state(&self.repo.borrow())
		);
	}

	pub fn abort_rebase(&self) {
		try_or_popup!(
			self,
			"abort rebase",
			sync::abort_pending_rebase(&self.repo.borrow())
		);
	}

	fn continue_rebase(&self) {
		try_or_popup!(
			self,
			"continue rebase",
			sync::continue_pending_rebase(&self.repo.borrow())
		);
	}

	fn commands_nav(
		&self,
		out: &mut Vec<CommandInfo>,
		force_all: bool,
	) {
		let focus_on_diff = self.is_focus_on_diff();
		out.push(
			CommandInfo::new(
				strings::commands::diff_focus_left(&self.key_config),
				true,
				(self.visible && focus_on_diff) || force_all,
			)
			.order(strings::order::NAV),
		);
		out.push(
			CommandInfo::new(
				strings::commands::diff_focus_right(&self.key_config),
				self.can_focus_diff(),
				(self.visible && !focus_on_diff) || force_all,
			)
			.order(strings::order::NAV),
		);
		out.push(
			CommandInfo::new(
				strings::commands::select_staging(&self.key_config),
				!focus_on_diff,
				(self.visible
					&& !focus_on_diff && self.focus == Focus::WorkDir)
					|| force_all,
			)
			.order(strings::order::NAV),
		);
		out.push(
			CommandInfo::new(
				strings::commands::select_unstaged(&self.key_config),
				!focus_on_diff,
				(self.visible
					&& !focus_on_diff && self.focus == Focus::Stage)
					|| force_all,
			)
			.order(strings::order::NAV),
		);
	}

	fn can_commit(&self) -> bool {
		self.index.focused()
			&& !self.index.is_empty()
			&& !self.pending_rebase()
	}
}

impl Component for Status {
	fn commands(
		&self,
		out: &mut Vec<CommandInfo>,
		force_all: bool,
	) -> CommandBlocking {
		let focus_on_diff = self.is_focus_on_diff();

		if self.visible || force_all {
			command_pump(
				out,
				force_all,
				self.components().as_slice(),
			);

			out.push(
				CommandInfo::new(
					strings::commands::commit_open(&self.key_config),
					true,
					self.can_commit() || force_all,
				)
				.order(-1),
			);

			out.push(CommandInfo::new(
				strings::commands::open_branch_select_popup(
					&self.key_config,
				),
				true,
				!focus_on_diff,
			));

			out.push(CommandInfo::new(
				strings::commands::status_push(&self.key_config),
				self.can_push(),
				!focus_on_diff,
			));
			out.push(CommandInfo::new(
				strings::commands::status_force_push(
					&self.key_config,
				),
				true,
				self.can_push() && !focus_on_diff,
			));
			out.push(CommandInfo::new(
				strings::commands::status_pull(&self.key_config),
				self.can_pull(),
				!focus_on_diff,
			));

			out.push(CommandInfo::new(
				strings::commands::undo_commit(&self.key_config),
				true,
				(!self.pending_rebase() && !focus_on_diff)
					|| force_all,
			));

			out.push(CommandInfo::new(
				strings::commands::abort_merge(&self.key_config),
				true,
				self.can_abort_merge() || force_all,
			));

			out.push(CommandInfo::new(
				strings::commands::continue_rebase(&self.key_config),
				true,
				self.pending_rebase() || force_all,
			));

			out.push(CommandInfo::new(
				strings::commands::abort_rebase(&self.key_config),
				true,
				self.pending_rebase() || force_all,
			));

			out.push(CommandInfo::new(
				strings::commands::abort_revert(&self.key_config),
				true,
				self.pending_revert() || force_all,
			));

			out.push(CommandInfo::new(
				strings::commands::view_submodules(&self.key_config),
				true,
				true,
			));
		}

		{
			out.push(CommandInfo::new(
				strings::commands::edit_item(&self.key_config),
				if focus_on_diff {
					true
				} else {
					self.can_focus_diff()
				},
				self.visible || force_all,
			));

			self.commands_nav(out, force_all);
		}

		visibility_blocking(self)
	}

	#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
	fn event(
		&mut self,
		ev: &crossterm::event::Event,
	) -> Result<EventState> {
		if self.visible {
			if event_pump(ev, self.components_mut().as_mut_slice())?
				.is_consumed()
			{
				self.git_action_executed = true;
				return Ok(EventState::Consumed);
			}

			if let Event::Key(k) = ev {
				return if key_match(k, self.key_config.keys.edit_file)
					&& (self.can_focus_diff()
						|| self.is_focus_on_diff())
				{
					if let Some((path, _)) = self.selected_path() {
						self.queue.push(
							InternalEvent::OpenExternalEditor(Some(
								path,
							)),
						);
					}
					Ok(EventState::Consumed)
				} else if key_match(
					k,
					self.key_config.keys.open_commit,
				) && self.can_commit()
				{
					self.queue.push(InternalEvent::OpenCommit);
					Ok(EventState::Consumed)
				} else if key_match(
					k,
					self.key_config.keys.toggle_workarea,
				) && !self.is_focus_on_diff()
				{
					self.switch_focus(self.focus.toggled_focus())
						.map(Into::into)
				} else if key_match(
					k,
					self.key_config.keys.focus_right,
				) && self.can_focus_diff()
				{
					self.switch_focus(Focus::Diff).map(Into::into)
				} else if key_match(
					k,
					self.key_config.keys.focus_left,
				) {
					self.switch_focus(match self.diff_target {
						DiffTarget::Stage => Focus::Stage,
						DiffTarget::WorkingDir => Focus::WorkDir,
					})
					.map(Into::into)
				} else if key_match(k, self.key_config.keys.move_down)
					&& self.focus == Focus::WorkDir
					&& !self.index.is_empty()
				{
					self.switch_focus(Focus::Stage).map(Into::into)
				} else if key_match(k, self.key_config.keys.move_up)
					&& self.focus == Focus::Stage
					&& !self.index_wd.is_empty()
				{
					self.switch_focus(Focus::WorkDir).map(Into::into)
				} else if key_match(
					k,
					self.key_config.keys.select_branch,
				) && !self.is_focus_on_diff()
				{
					self.queue.push(InternalEvent::SelectBranch);
					Ok(EventState::Consumed)
				} else if key_match(
					k,
					self.key_config.keys.force_push,
				) && !self.is_focus_on_diff()
					&& self.can_push()
				{
					self.push(true);
					Ok(EventState::Consumed)
				} else if key_match(k, self.key_config.keys.push)
					&& !self.is_focus_on_diff()
				{
					self.push(false);
					Ok(EventState::Consumed)
				} else if key_match(k, self.key_config.keys.pull)
					&& !self.is_focus_on_diff()
					&& self.can_pull()
				{
					self.pull();
					Ok(EventState::Consumed)
				} else if key_match(
					k,
					self.key_config.keys.undo_commit,
				) && !self.is_focus_on_diff()
				{
					self.undo_last_commit();
					self.queue.push(InternalEvent::Update(
						NeedsUpdate::ALL,
					));
					Ok(EventState::Consumed)
				} else if key_match(
					k,
					self.key_config.keys.abort_merge,
				) {
					if self.can_abort_merge() {
						self.queue.push(
							InternalEvent::ConfirmAction(
								Action::AbortMerge,
							),
						);
					} else if self.pending_rebase() {
						self.queue.push(
							InternalEvent::ConfirmAction(
								Action::AbortRebase,
							),
						);
					} else if self.pending_revert() {
						self.queue.push(
							InternalEvent::ConfirmAction(
								Action::AbortRevert,
							),
						);
					}

					Ok(EventState::Consumed)
				} else if key_match(
					k,
					self.key_config.keys.rebase_branch,
				) && self.pending_rebase()
				{
					self.continue_rebase();
					self.queue.push(InternalEvent::Update(
						NeedsUpdate::ALL,
					));
					Ok(EventState::Consumed)
				} else if key_match(
					k,
					self.key_config.keys.view_submodules,
				) {
					self.queue.push(InternalEvent::ViewSubmodules);
					Ok(EventState::Consumed)
				} else {
					Ok(EventState::NotConsumed)
				};
			}
		}

		Ok(EventState::NotConsumed)
	}

	fn is_visible(&self) -> bool {
		self.visible
	}

	fn hide(&mut self) {
		self.visible = false;
	}

	fn show(&mut self) -> Result<()> {
		self.visible = true;
		self.check_remotes();
		self.update()?;

		Ok(())
	}
}
