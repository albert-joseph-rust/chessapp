use std::cmp::{min, max};
use std::rc::Rc;
use std::cell::RefCell;
use std::f64::consts::PI;

use shakmaty::{Square, Color, Role, Piece, Board, Bitboard, MoveList, Position, Chess};

use gtk;
use gtk::prelude::*;
use gtk::DrawingArea;
use gdk;
use gdk::{EventButton, EventMotion};
use cairo;
use cairo::prelude::*;
use cairo::{Context, RadialGradient};
use rsvg::HandleExt;

use option_filter::OptionFilterExt;

use time::SteadyTime;

use relm::{Relm, Widget, Update, EventStream};

use util;
use pieceset;
use drawable::Drawable;
use promotable::Promotable;
use pieceset::PieceSet;

pub struct Model {
    state: Rc<RefCell<State>>,
}

#[derive(Msg)]
pub enum GroundMsg {
    SetPosition {
        board: Board,
        legals: MoveList,
        last_move: Option<(Square, Square)>,
        check: Option<Square>,
    },
    UserMove(Square, Square, Option<Role>),
    ShapesChanged,
}

pub struct Ground {
    drawing_area: DrawingArea,
    model: Model,
}

impl Update for Ground {
    type Model = Model;
    type ModelParam = ();
    type Msg = GroundMsg;

    fn model(_: &Relm<Self>, _: ()) -> Model {
        Model {
            state: Rc::new(RefCell::new(State::new())),
        }
    }

    fn update(&mut self, event: GroundMsg) {
        let mut state = self.model.state.borrow_mut();

        match event {
            GroundMsg::UserMove(orig, dest, None) if state.board_state.valid_move(orig, dest) => {
                if state.board_state.legals.iter().any(|m| m.from() == Some(orig) && m.to() == dest && m.promotion().is_some()) {
                    state.promotable.start_promoting(orig, dest);
                    self.drawing_area.queue_draw();
                }
            },
            GroundMsg::SetPosition { board, legals, last_move, check } => {
                state.board_state.pieces.set_board(board);
                state.board_state.legals = legals;
                state.board_state.last_move = last_move;
                state.board_state.check = check;

                self.drawing_area.queue_draw();
            },
            _ => {}
        }
    }
}

impl Widget for Ground {
    type Root = DrawingArea;

    fn root(&self) -> Self::Root {
        self.drawing_area.clone()
    }

    fn view(relm: &Relm<Self>, model: Model) -> Self {
        let drawing_area = DrawingArea::new();

        drawing_area.add_events((gdk::BUTTON_PRESS_MASK |
                                 gdk::BUTTON_RELEASE_MASK |
                                 gdk::POINTER_MOTION_MASK).bits() as i32);

        {
            let weak_state = Rc::downgrade(&model.state);
            drawing_area.connect_draw(move |widget, cr| {
                if let Some(state) = weak_state.upgrade() {
                    let mut state = state.borrow_mut();
                    state.board_state.now = SteadyTime::now();

                    let animating = state.board_state.pieces.is_animating(state.board_state.now) ||
                                    state.promotable.is_animating();

                    let matrix = util::compute_matrix(widget, state.board_state.orientation);
                    cr.set_matrix(matrix);

                    draw_border(cr, &state.board_state);
                    draw_board(cr, &state.board_state);
                    draw_check(cr, &state.board_state);
                    state.board_state.pieces.render(cr, &state.board_state, &state.promotable);
                    state.drawable.draw(cr);
                    draw_move_hints(cr, &state.board_state);
                    draw_drag(cr, &state.board_state);
                    state.promotable.draw(cr, &state.board_state);

                    let weak_state = weak_state.clone();
                    let widget = widget.clone();
                    if animating {
                        gtk::idle_add(move || {
                            if let Some(state) = weak_state.upgrade() {
                                let state = state.borrow();
                                state.board_state.pieces.queue_animation(&state.board_state, &widget);
                                state.promotable.queue_animation(&state.board_state, &widget);
                            }
                            Continue(false)
                        });
                    }
                }
                Inhibit(false)
            });
        }

        {
            let state = Rc::downgrade(&model.state);
            let stream = relm.stream().clone();
            drawing_area.connect_button_press_event(move |widget, e| {
                if let Some(state) = state.upgrade() {
                    let mut state = state.borrow_mut();

                    let ctx = EventContext {
                        drawing_area: &widget,
                        stream: &stream,
                        pos: e.get_position(),
                        square: util::pos_to_square(widget, state.board_state.orientation, e.get_position()),
                    };

                    button_press_event(&mut state, &ctx, e);
                }
                Inhibit(false)
            });
        }

        {
            let state = Rc::downgrade(&model.state);
            let stream = relm.stream().clone();
            drawing_area.connect_button_release_event(move |widget, e| {
                if let Some(state) = state.upgrade() {
                    let mut state = state.borrow_mut();

                    let ctx = EventContext {
                        drawing_area: widget,
                        stream: &stream,
                        pos: e.get_position(),
                        square: util::pos_to_square(widget, state.board_state.orientation, e.get_position()),
                    };

                    state.board_state.drag_mouse_up(&ctx);
                    state.drawable.mouse_up(&ctx);
                }
                Inhibit(false)
            });
        }

        {
            let state = Rc::downgrade(&model.state);
            let stream = relm.stream().clone();
            drawing_area.connect_motion_notify_event(move |widget, e| {
                if let Some(state) = state.upgrade() {
                    let mut state = state.borrow_mut();

                    let ctx = EventContext {
                        drawing_area: widget,
                        stream: &stream,
                        pos: e.get_position(),
                        square: util::pos_to_square(widget, state.board_state.orientation, e.get_position()),
                    };

                    motion_notify_event(&mut state, &ctx, e);
                }
                Inhibit(false)
            });
        }

        drawing_area.set_hexpand(true);
        drawing_area.set_vexpand(true);
        drawing_area.show();

        Ground {
            drawing_area,
            model,
        }
    }
}

fn motion_notify_event(state: &mut State, ctx: &EventContext, e: &EventMotion) {
    if !state.promotable.mouse_move(&state.board_state, &ctx) {
        drag_mouse_move(&mut state.board_state, ctx.drawing_area, ctx.square, e);
        state.drawable.mouse_move(&ctx);
    }
}

fn button_press_event(state: &mut State, ctx: &EventContext, e: &EventButton) {
    let promotable = &mut state.promotable;
    let board_state = &mut state.board_state;

    if !promotable.mouse_down(board_state, &ctx) {
        board_state.selection_mouse_down(&ctx, e);
        drag_mouse_down(board_state, ctx.drawing_area, ctx.square, e);
        state.drawable.mouse_down(&ctx, e);
    }
}

struct State {
    board_state: BoardState,
    drawable: Drawable,
    promotable: Promotable,
}

impl State {
    fn new() -> State {
        State {
            board_state: BoardState::new(),
            drawable: Drawable::new(),
            promotable: Promotable::new(),
        }
    }
}

pub struct EventContext<'a> {
    pub drawing_area: &'a DrawingArea,
    pub stream: &'a EventStream<GroundMsg>,
    pub pos: (f64, f64),
    pub square: Option<Square>,
}

pub const ANIMATE_DURATION: f64 = 0.2;

fn ease_in_out_cubic(start: f64, end: f64, elapsed: f64, duration: f64) -> f64 {
    let t = elapsed / duration;
    let ease = if t >= 1.0 {
        1.0
    } else if t >= 0.5 {
        (t - 1.0) * (2.0 * t - 2.0) * (2.0 * t - 2.0) + 1.0
    } else if t >= 0.0 {
        4.0 * t * t * t
    } else {
        0.0
    };
    start + (end - start) * ease
}

pub(crate) struct Figurine {
    square: Square,
    piece: Piece,
    pub(crate) pos: (f64, f64),
    pub(crate) time: SteadyTime,
    fading: bool,
    replaced: bool,
    dragging: bool,
}

impl Figurine {
    fn pos(&self, now: SteadyTime) -> (f64, f64) {
        let end = util::square_to_inverted(self.square);
        if self.dragging {
            end
        } else if self.fading {
            self.pos
        } else {
            (ease_in_out_cubic(self.pos.0, end.0, self.elapsed(now), ANIMATE_DURATION),
             ease_in_out_cubic(self.pos.1, end.1, self.elapsed(now), ANIMATE_DURATION))
        }
    }

    fn alpha(&self, now: SteadyTime) -> f64 {
        if self.dragging {
            0.2 * self.alpha_easing(1.0, now)
        } else {
            self.drag_alpha(now)
        }
    }

    fn drag_alpha(&self, now: SteadyTime) -> f64 {
        let base = if self.fading && self.replaced { 0.5 } else { 1.0 };
        self.alpha_easing(base, now)
    }

    fn alpha_easing(&self, base: f64, now: SteadyTime) -> f64 {
        if self.fading {
            base * ease_in_out_cubic(1.0, 0.0, self.elapsed(now), ANIMATE_DURATION)
        } else {
            base
        }
    }

    fn elapsed(&self, now: SteadyTime) -> f64 {
        (now - self.time).num_milliseconds() as f64 / 1000.0
    }

    fn is_animating(&self, now: SteadyTime) -> bool {
        !self.dragging && self.elapsed(now) <= ANIMATE_DURATION &&
        (self.fading || self.pos != util::square_to_inverted(self.square))
    }

    fn queue_animation(&self, state: &BoardState, widget: &DrawingArea) {
        if self.is_animating(state.now) {
            let matrix = util::compute_matrix(widget, state.orientation);
            let pos = self.pos(state.now);

            let (x1, y1) = matrix.transform_point(pos.0 - 0.5, pos.1 - 0.5);
            let (x2, y2) = matrix.transform_point(pos.0 + 0.5, pos.1 + 0.5);
            let (x3, y3) = matrix.transform_point(self.square.file() as f64, 7.0 - self.square.rank() as f64);
            let (x4, y4) = matrix.transform_point(1.0 + self.square.file() as f64, 8.0 - self.square.rank() as f64);

            let xmin = min(
                min(x1.floor() as i32, x2.floor() as i32),
                min(x3.floor() as i32, x4.floor() as i32));
            let xmax = max(
                max(x1.ceil() as i32, x2.ceil() as i32),
                max(x3.ceil() as i32, x4.ceil() as i32));
            let ymin = min(
                min(y1.floor() as i32, y2.floor() as i32),
                min(y3.floor() as i32, y4.floor() as i32));
            let ymax = max(
                max(y1.ceil() as i32, y2.ceil() as i32),
                max(y3.ceil() as i32, y4.ceil() as i32));

            widget.queue_draw_area(xmin, ymin, xmax - xmin, ymax - ymin);
        }
    }

    fn render(&self, cr: &Context, board_state: &BoardState, promotable: &Promotable) {
        // hide piece while promotion dialog is open
        if promotable.is_promoting(self.square) {
            return;
        }

        cr.push_group();

        let (x, y) = self.pos(board_state.now);
        cr.translate(x, y);
        cr.rotate(board_state.orientation.fold(0.0, PI));
        cr.translate(-0.5, -0.5);
        cr.scale(board_state.piece_set.scale(), board_state.piece_set.scale());

        board_state.piece_set.by_piece(&self.piece).render_cairo(cr);

        cr.pop_group_to_source();
        cr.paint_with_alpha(self.alpha(board_state.now));
    }
}

pub(crate) struct Pieces {
    board: Board,
    figurines: Vec<Figurine>,
}

impl Pieces {
    pub fn new() -> Pieces {
        Pieces::new_from_board(&Board::new())
    }

    pub fn new_from_board(board: &Board) -> Pieces {
        Pieces {
            board: board.clone(),
            figurines: board.pieces().map(|(square, piece)| Figurine {
                square,
                piece,
                pos: (0.5 + square.file() as f64, 7.5 - square.rank() as f64),
                time: SteadyTime::now(),
                fading: false,
                replaced: false,
                dragging: false,
            }).collect()
        }
    }

    pub fn set_board(&mut self, board: Board) {
        let now = SteadyTime::now();

        // clean and freeze previous animation
        self.figurines.retain(|f| f.alpha(now) > 0.0001);
        for figurine in &mut self.figurines {
            if !figurine.fading {
                figurine.pos = figurine.pos(now);
                figurine.time = now;
            }
        }

        // diff
        let mut removed = Bitboard(0);
        let mut added = Vec::new();

        for square in self.board.occupied() | board.occupied() {
            let old = self.board.piece_at(square);
            let new = board.piece_at(square);
            if old != new {
                if old.is_some() {
                    removed.add(square);
                }
                if let Some(new) = new {
                    added.push((square, new));
                }
            }
        }

        // try to match additions and removals
        let mut matched = Vec::new();
        added.retain(|&(square, piece)| {
            let best = removed
                .filter(|sq| self.board.by_piece(piece).contains(*sq))
                .min_by_key(|sq| sq.distance(square));

            if let Some(best) = best {
                removed.remove(best);
                matched.push((best, square));
                false
            } else {
                true
            }
        });

        for square in removed {
            for figurine in &mut self.figurines {
                if !figurine.fading && figurine.square == square {
                    figurine.fading = true;
                    figurine.replaced = board.occupied().contains(square);
                    figurine.time = now;
                }
            }
        }

        for (orig, dest) in matched {
            if let Some(figurine) = self.figurines.iter_mut().find(|f| !f.fading && f.square == orig) {
                figurine.square = dest;
                figurine.time = now;
            }
        }

        for (square, piece) in added {
            self.figurines.push(Figurine {
                square: square,
                piece: piece,
                pos: (0.5 + square.file() as f64, 7.5 - square.rank() as f64),
                time: now,
                fading: false,
                replaced: false,
                dragging: false,
            });
        }

        self.board = board;
    }

    pub fn occupied(&self) -> Bitboard {
        self.board.occupied()
    }

    pub fn render(&self, cr: &Context, state: &BoardState, promotable: &Promotable) {
        let now = SteadyTime::now();

        for figurine in &self.figurines {
            if figurine.fading {
                figurine.render(cr, state, promotable);
            }
        }

        for figurine in &self.figurines {
            if !figurine.fading && !figurine.is_animating(now) {
                figurine.render(cr, state, promotable);
            }
        }

        for figurine in &self.figurines {
            if !figurine.fading && figurine.is_animating(now) {
                figurine.render(cr, state, promotable);
            }
        }
    }

    pub fn figurine_at(&self, square: Square) -> Option<&Figurine> {
        self.figurines.iter().find(|f| !f.fading && f.square == square)
    }

    pub fn figurine_at_mut(&mut self, square: Square) -> Option<&mut Figurine> {
        self.figurines.iter_mut().find(|f| !f.fading && f.square == square)
    }

    pub fn dragging(&self) -> Option<&Figurine> {
        self.figurines.iter().find(|f| f.dragging)
    }

    pub fn dragging_mut(&mut self) -> Option<&mut Figurine> {
        self.figurines.iter_mut().find(|f| f.dragging)
    }

    pub fn is_animating(&self, now: SteadyTime) -> bool {
        self.figurines.iter().any(|f| f.is_animating(now))
    }

    pub fn queue_animation(&self, state: &BoardState, widget: &DrawingArea) {
        for figurine in &self.figurines {
            figurine.queue_animation(state, widget);
        }
    }
}

struct DragStart {
    pos: (f64, f64),
    square: Square,
}

pub(crate) struct BoardState {
    pub(crate) pieces: Pieces,
    orientation: Color,
    check: Option<Square>,
    selected: Option<Square>,
    last_move: Option<(Square, Square)>,
    drag_start: Option<DragStart>,
    piece_set: PieceSet,
    now: SteadyTime,
    legals: MoveList,
}

impl BoardState {
    fn move_targets(&self, orig: Square) -> Bitboard {
        self.legals.iter().filter(|m| m.from() == Some(orig)).map(|m| m.to()).collect()
    }

    fn valid_move(&self, orig: Square, dest: Square) -> bool {
        self.move_targets(orig).contains(dest)
    }
}

impl BoardState {
    fn new() -> Self {
        let pos = Chess::default();
        let mut legals = MoveList::new();
        pos.legal_moves(&mut legals);

        BoardState {
            pieces: Pieces::new(),
            orientation: Color::White,
            check: None,
            last_move: None,
            selected: None,
            drag_start: None,
            piece_set: pieceset::PieceSet::merida(),
            legals,
            now: SteadyTime::now(),
        }
    }
}

impl BoardState {
    fn selection_mouse_down(&mut self, context: &EventContext, e: &EventButton) {
        let orig = self.selected.take();

        if e.get_button() == 1 {
            let dest = context.square;
            self.selected = dest.filter(|sq| self.pieces.occupied().contains(*sq));

            if let (Some(orig), Some(dest)) = (orig, dest) {
                self.selected = None;
                if orig != dest {
                    context.stream.emit(GroundMsg::UserMove(orig, dest, None));
                }
            }
        }

        context.drawing_area.queue_draw();
    }
}

fn drag_mouse_down(state: &mut BoardState, widget: &DrawingArea, square: Option<Square>, e: &EventButton) {
    if e.get_button() == 1 {
        if let Some(square) = square {
            if state.pieces.figurine_at(square).is_some() {
                state.drag_start = Some(DragStart {
                    pos: util::invert_pos(widget, state.orientation, e.get_position()),
                    square,
                });
            }
        }
    }
}

fn queue_draw_square(widget: &DrawingArea, orientation: Color, square: Square) {
    queue_draw_rect(widget, orientation, square.file() as f64, 7.0 - square.rank() as f64, 1.0, 1.0);
}

fn queue_draw_rect(widget: &DrawingArea, orientation: Color, x: f64, y: f64, width: f64, height: f64) {
    let matrix = util::compute_matrix(widget, orientation);
    let (x1, y1) = matrix.transform_point(x, y);
    let (x2, y2) = matrix.transform_point(x + width, y + height);

    let xmin = min(x1.floor() as i32, x2.floor() as i32);
    let ymin = min(y1.floor() as i32, y2.floor() as i32);
    let xmax = max(x1.ceil() as i32, x2.ceil() as i32);
    let ymax = max(y1.ceil() as i32, y2.ceil() as i32);

    widget.queue_draw_area(xmin, ymin, xmax - xmin, ymax - ymin);
}

fn drag_mouse_move(state: &mut BoardState, widget: &DrawingArea, square: Option<Square>, e: &EventMotion) {
    let pos = util::invert_pos(widget, state.orientation, e.get_position());

    if let Some(ref drag_start) = state.drag_start {
        let drag_distance = (drag_start.pos.0 - pos.0).hypot(drag_start.pos.1 - pos.1);
        if drag_distance >= 0.1 {
            if let Some(dragging) = state.pieces.figurine_at_mut(drag_start.square) {
                dragging.dragging = true;
            }
        }
    }

    if let Some(dragging) = state.pieces.dragging_mut() {
        // ensure orig square is selected
        if state.selected != Some(dragging.square) {
            state.selected = Some(dragging.square);
            widget.queue_draw();
        }

        // invalidate previous
        queue_draw_rect(widget, state.orientation, dragging.pos.0 - 0.5, dragging.pos.1 - 0.5, 1.0, 1.0);
        queue_draw_square(widget, state.orientation, dragging.square);
        if let Some(sq) = util::inverted_to_square(dragging.pos) {
            queue_draw_square(widget, state.orientation, sq);
        }

        // update position
        dragging.pos = pos;
        dragging.time = SteadyTime::now();

        // invalidate new
        queue_draw_rect(widget, state.orientation, dragging.pos.0 - 0.5, dragging.pos.1 - 0.5, 1.0, 1.0);
        if let Some(sq) = square {
            queue_draw_square(widget, state.orientation, sq);
        }
    }
}

impl BoardState {
    fn drag_mouse_up(&mut self, context: &EventContext) {
        self.drag_start = None;

        let m = if let Some(dragging) = self.pieces.dragging_mut() {
            context.drawing_area.queue_draw();

            let dest = context.square.unwrap_or(dragging.square);
            dragging.pos = util::square_to_inverted(dest);
            dragging.time = SteadyTime::now();
            dragging.dragging = false;

            if dragging.square != dest && !dragging.fading {
                self.selected = None;
                Some((dragging.square, dest))
            } else {
                None
            }
        } else {
            None
        };

        if let Some((orig, dest)) = m {
            if orig != dest {
                context.stream.emit(GroundMsg::UserMove(orig, dest, None));
            }
        }
    }
}

fn draw_text(cr: &Context, orientation: Color, (x, y): (f64, f64), text: &str) {
    let font = cr.font_extents();
    let e = cr.text_extents(text);

    cr.save();
    cr.translate(x, y);
    cr.rotate(orientation.fold(0.0, PI));
    cr.move_to(-0.5 * e.width, 0.5 * font.ascent);
    cr.show_text(text);
    cr.restore();
}

fn draw_border(cr: &Context, state: &BoardState) {
    let border = cairo::SolidPattern::from_rgb(0.2, 0.2, 0.5);
    cr.set_source(&border);
    cr.rectangle(-0.5, -0.5, 9.0, 9.0);
    cr.fill();

    cr.set_font_size(0.20);
    cr.set_source_rgb(0.8, 0.8, 0.8);

    for (rank, glyph) in ["1", "2", "3", "4", "5", "6", "7", "8"].iter().enumerate() {
        draw_text(cr, state.orientation, (-0.25, 7.5 - rank as f64), glyph);
        draw_text(cr, state.orientation, (8.25, 7.5 - rank as f64), glyph);
    }

    for (file, glyph) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
        draw_text(cr, state.orientation, (0.5 + file as f64, -0.25), glyph);
        draw_text(cr, state.orientation, (0.5 + file as f64, 8.25), glyph);
    }
}

fn draw_board(cr: &Context, state: &BoardState) {
    let light = cairo::SolidPattern::from_rgb(0.87, 0.89, 0.90);
    let dark = cairo::SolidPattern::from_rgb(0.55, 0.64, 0.68);

    cr.rectangle(0.0, 0.0, 8.0, 8.0);
    cr.set_source(&dark);
    cr.fill();

    cr.set_source(&light);

    for square in Bitboard::all() {
        if square.is_light() {
            cr.rectangle(square.file() as f64, 7.0 - square.rank() as f64, 1.0, 1.0);
            cr.fill();
        }
    }

    if let Some(selected) = state.selected {
        cr.rectangle(selected.file() as f64, 7.0 - selected.rank() as f64, 1.0, 1.0);
        cr.set_source_rgba(0.08, 0.47, 0.11, 0.5);
        cr.fill();

        if let Some(hovered) = state.pieces.dragging().and_then(|d| util::inverted_to_square(d.pos)) {
            if state.valid_move(selected, hovered) {
                cr.rectangle(hovered.file() as f64, 7.0 - hovered.rank() as f64, 1.0, 1.0);
                cr.set_source_rgba(0.08, 0.47, 0.11, 0.25);
                cr.fill();
            }
        }
    }

    if let Some((orig, dest)) = state.last_move {
        cr.set_source_rgba(0.61, 0.78, 0.0, 0.41);
        cr.rectangle(orig.file() as f64, 7.0 - orig.rank() as f64, 1.0, 1.0);
        cr.fill();

        if dest != orig {
            cr.rectangle(dest.file() as f64, 7.0 - dest.rank() as f64, 1.0, 1.0);
            cr.fill();
        }
    }
}

fn draw_move_hints(cr: &Context, state: &BoardState) {
    if let Some(selected) = state.selected {
        cr.set_source_rgba(0.08, 0.47, 0.11, 0.5);

        let radius = 0.12;
        let corner = 1.8 * radius;

        for square in state.move_targets(selected) {
            if state.pieces.occupied().contains(square) {
                cr.move_to(square.file() as f64, 7.0 - square.rank() as f64);
                cr.rel_line_to(corner, 0.0);
                cr.rel_line_to(-corner, corner);
                cr.rel_line_to(0.0, -corner);
                cr.fill();

                cr.move_to(1.0 + square.file() as f64, 7.0 - square.rank() as f64);
                cr.rel_line_to(0.0, corner);
                cr.rel_line_to(-corner, -corner);
                cr.rel_line_to(corner, 0.0);
                cr.fill();

                cr.move_to(square.file() as f64, 8.0 - square.rank() as f64);
                cr.rel_line_to(corner, 0.0);
                cr.rel_line_to(-corner, -corner);
                cr.rel_line_to(0.0, corner);
                cr.fill();

                cr.move_to(1.0 + square.file() as f64, 8.0 - square.rank() as f64);
                cr.rel_line_to(-corner, 0.0);
                cr.rel_line_to(corner, -corner);
                cr.rel_line_to(0.0, corner);
                cr.fill();
            } else {
                cr.arc(0.5 + square.file() as f64,
                       7.5 - square.rank() as f64,
                       radius, 0.0, 2.0 * PI);
                cr.fill();
            }
        }
    }
}

fn draw_check(cr: &Context, state: &BoardState) {
    if let Some(check) = state.check {
        let cx = 0.5 + check.file() as f64;
        let cy = 7.5 - check.rank() as f64;
        let gradient = RadialGradient::new(cx, cy, 0.0, cx, cy, 0.5f64.hypot(0.5));
        gradient.add_color_stop_rgba(0.0, 1.0, 0.0, 0.0, 1.0);
        gradient.add_color_stop_rgba(0.25, 0.91, 0.0, 0.0, 1.0);
        gradient.add_color_stop_rgba(0.89, 0.66, 0.0, 0.0, 0.0);
        cr.set_source(&gradient);
        cr.paint();
    }
}

fn draw_drag(cr: &Context, state: &BoardState) {
    if let Some(dragging) = state.pieces.dragging() {
        cr.push_group();
        cr.translate(dragging.pos.0, dragging.pos.1);
        cr.rotate(state.orientation.fold(0.0, PI));
        cr.translate(-0.5, -0.5);
        cr.scale(state.piece_set.scale(), state.piece_set.scale());
        state.piece_set.by_piece(&dragging.piece).render_cairo(cr);
        cr.pop_group_to_source();
        cr.paint_with_alpha(dragging.drag_alpha(state.now));
    }
}
