/**
 * The surface and clock ports the compositor draws through (SL-4.UI.03).
 *
 * Why these are ports and not a canvas: the compositor's job is *deciding what
 * to draw* — which tile, at which device scale, into which box, and whether it
 * is a zoom blit standing in for a sharper tile. Where those pixels land is
 * host-specific, and differs materially between hosts:
 *
 * - **Web**: the canvas is handed to the render worker once with
 *   `transferControlToOffscreen()` and every frame is drawn *in the worker*;
 *   the worker returns the composed frame with `transferToImageBitmap()` in the
 *   message's transfer list. Nothing is copied in either direction.
 * - **Extension (MV3)**: the same shape, hosted by the offscreen document
 *   (`apps/extension/offscreen.html`) rather than a dedicated worker.
 * - **Desktop (Tauri v2)**: the webview's own 2D context on the UI thread; no
 *   worker, and the tile payload stays an `ArrayBuffer`.
 * - **Tests**: an in-memory recorder that captures frames as plain data.
 *
 * All four are the same three operations, so the compositor names only these
 * interfaces. That is also what keeps `platform-globals.test.ts` green: this
 * package still touches no `OffscreenCanvas`, no `Worker`, and no
 * `requestAnimationFrame` — the seam holds where UI.01 put it, and the
 * compositor does not become a second way around it.
 */

/**
 * A destination the compositor can present frames to.
 *
 * **Ownership.** An implementation may take ownership of the payload buffers
 * reachable from {@link DrawOp.source} — that is the point of the transfer
 * (a tile rendered in the worker is never copied to the UI thread, and a frame
 * composited in the worker is never copied back). The contract the compositor
 * honours in exchange: after `draw` returns for a frame, it reads **only
 * identity and geometry** from those entries (`page`, `stage`, `deviceScale`,
 * and the tile's own pixel dimensions) and never touches the pixel buffer
 * again. Identity and dimensions are recorded on the entry itself, so they
 * survive a detached buffer.
 */
export interface TileSurface {
	/**
	 * True when this surface takes ownership of tile payload buffers (the
	 * worker path: the buffer is transferred, not copied). A main-thread
	 * surface leaves this false and may read the payload for as long as it
	 * likes.
	 */
	readonly takesOwnership: boolean;

	/**
	 * Size the backing store. `deviceWidth`/`deviceHeight` are whole device
	 * pixels — the compositor rounds, because a canvas backing store with a
	 * fractional dimension is either rejected or silently floored, and the
	 * floor is the classic fractional-DPR bug: the right-hand column of the
	 * surface is never painted.
	 *
	 * Called only when the size actually changes, so a resize storm does not
	 * reallocate the backing store every frame.
	 */
	configure(size: SurfaceSize): void;

	/**
	 * Draw one frame. Called at most once per animation frame, always after
	 * the `configure` that matches it, and always with every op in
	 * document order. The frame carries `clear: true` and the surface is
	 * expected to honour it.
	 */
	draw(frame: CompositorFrame): void;

	/**
	 * Present whatever was drawn. On the worker path this is where
	 * `transferToImageBitmap()` hands the composed frame back for display; on a
	 * main-thread context it is a no-op, because drawing to the context is
	 * already the presentation.
	 */
	present(): void;

	/** Release the backing store. Idempotent. */
	dispose(): void;
}

/** The surface's backing store, in device pixels, plus the facts it was sized from. */
export interface SurfaceSize {
	/** Viewport width in CSS pixels. */
	readonly cssWidth: number;
	/** Viewport height in CSS pixels. */
	readonly cssHeight: number;
	/** Whole device pixels across; `Math.round(cssWidth * dpr)`, at least 1. */
	readonly deviceWidth: number;
	/** Whole device pixels down; `Math.round(cssHeight * dpr)`, at least 1. */
	readonly deviceHeight: number;
	/**
	 * Device pixels per CSS pixel **actually in force** — the backing store's
	 * width divided by the CSS width, not the nominal `devicePixelRatio`. At
	 * DPR 1.25 a 1013-CSS-px viewport rounds to 1266 device px, so the real
	 * factor is 1.2497…, and drawing with 1.25 leaves a 0.25-px seam down the
	 * right-hand side. Everything the compositor converts uses this number.
	 */
	readonly scale: number;
	/** The nominal ratio, kept for diagnostics and for the ladder's requests. */
	readonly devicePixelRatio: number;
}

/**
 * Why a surface call was refused.
 *
 * A code, not a sentence, and deliberately so. Two reasons: a code is what a
 * host maps to a *localised* message (the i18n rule applies to these as much as
 * to a page label), and a code is what a test asserts on. A prose message here
 * would be an English literal in the geometry layer that a translator cannot
 * reach and a rename would turn into a code change — the exact thing the
 * `strings.test.ts` gate exists to prevent.
 */
export type SurfaceFault =
	/** `attach` was called twice: a canvas's control transfers exactly once. */
	| "already-attached"
	/** `configure` before `attach`: the worker has no surface to size. */
	| "not-attached"
	/** `draw` before any `configure`: the worker has no backing store. */
	| "not-configured"
	/** The transferred canvas would not give a 2D context. */
	| "no-2d-context";

/** A surface call that breaks the protocol. Carries a {@link SurfaceFault} code. */
export class SurfaceFaultError extends Error {
	readonly fault: SurfaceFault;

	constructor(fault: SurfaceFault) {
		super(fault);
		this.name = "SurfaceFaultError";
		this.fault = fault;
	}
}

/** One tile to draw, in surface device pixels. */
export interface DrawOp {
	/** Document page this op paints. */
	readonly page: number;
	/** Which rung of the ladder the pixels came from. */
	readonly stage: "lowres" | "full";
	/** Device pixels per PDF point the bitmap was rasterised at. */
	readonly deviceScale: number;
	/** Device pixels per PDF point the surface is drawing at right now. */
	readonly targetDeviceScale: number;
	/** Source bitmap size, device pixels. */
	readonly sourceWidth: number;
	readonly sourceHeight: number;
	/** Destination box in surface device pixels; edges are whole numbers. */
	readonly x: number;
	readonly y: number;
	readonly width: number;
	readonly height: number;
	/**
	 * True when the bitmap is not being presented at its own resolution — a
	 * low-res rung, a zoom blit, or both. The surface turns smoothing off for
	 * these, which is what makes a stretched tile read as "not yet sharp"
	 * rather than as a smooth blur.
	 */
	readonly scaled: boolean;
	/**
	 * True when this op is standing in for a tile at the current scale that
	 * has not been rasterised yet. This is the zoom path: the previous scale's
	 * tile is presented *now*, at the new scale, and refined when the engine
	 * catches up. A frame with no provisional op and no placeholder op is a
	 * frame where nothing is standing in for anything.
	 */
	readonly provisional: boolean;
	/**
	 * The pixels. An opaque handle from the caller's point of view — an
	 * `ImageBitmap` for a worker surface, the `RenderedTile` for a main-thread
	 * one. See {@link TileSurface.takesOwnership} for who owns it.
	 */
	readonly source: unknown;
}

/** One frame's worth of work: everything the surface needs, and nothing else. */
export interface CompositorFrame {
	/** Monotonic frame counter; frame 0 is the first one drawn. */
	readonly index: number;
	/** Clock timestamp the frame was built for. */
	readonly at: number;
	/** The backing store this frame is sized for. */
	readonly size: SurfaceSize;
	/** Document scroll offset the boxes are relative to, CSS pixels. */
	readonly scrollTop: number;
	/**
	 * Clear before drawing. The compositor repaints the whole viewport every
	 * frame (the window is a dozen tiles), so a partial update would need a
	 * damage tracker — more state, more ways to be wrong, and no measurable
	 * win at this tile count.
	 */
	readonly clear: true;
	/** Draw ops in document order. */
	readonly ops: readonly DrawOp[];
	/** How many windowed pages had no usable pixels at all this frame. */
	readonly placeholders: number;
}

/** A cancelable frame request. */
export interface FrameHandle {
	readonly cancelled: boolean;
}

/**
 * The animation-frame clock. `requestAnimationFrame` is a platform global, so
 * the host supplies it; the compositor only ever *coalesces* on it, which is
 * the property the 16 ms budget rests on (a scroll event storm must not turn
 * into a draw per event).
 */
export interface FrameClock {
	/** Run `callback(timestampMs)` on the next frame. */
	requestFrame(callback: (timestampMs: number) => void): FrameHandle;
	/** Drop a pending request. Cancelling an already-run handle is a no-op. */
	cancelFrame(handle: FrameHandle): void;
}
