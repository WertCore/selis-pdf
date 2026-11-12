/**
 * The command palette: every command in the workspace, in one box.
 *
 * ## Why it is a view over the ribbon
 *
 * The ribbon's six tabs and the inspector's panels put a lot of commands a
 * long way from each other, and the list keeps growing. A box the reader
 * can type into reaches all of them in one place - which is what every tool
 * that has passed a handful of commands does.
 *
 * The commands are **read out of the markup** rather than listed again
 * here. That is the whole design: a command that is in the ribbon is in the
 * palette, with the same label and the same behaviour, and a new command
 * needs no second list to be kept in step with the first. The palette runs
 * the ribbon's own button, so the two cannot drift.
 *
 * ## Why there is no DOM in here
 *
 * The rules say the view-model names no platform global, so its tests run in
 * Node (ADR-P0044). The shell reads the ribbon's buttons and hands the
 * palette plain data; everything here is a function of that data.
 */

/** One command the palette can run. */
export interface PaletteCommand {
	/** What the reader sees: the button's own label. */
	readonly label: string;
	/** Where the command lives: the ribbon group's name. */
	readonly where: string;
	/** The button's icon, when it has one. */
	readonly icon: string | null;
	/** The action the button carries. */
	readonly action: string;
}

/**
 * A button, as far as the palette is concerned.
 *
 * The shell reads these out of the ribbon's markup; the palette never
 * touches the DOM itself.
 */
export interface CommandSource {
	readonly label: string;
	readonly where: string;
	readonly icon: string | null;
	readonly action: string;
}

/**
 * The commands, from the buttons.
 *
 * A button with no label is not a command the reader could have asked for,
 * so it is dropped; everything else is kept, including a command whose
 * action the shell may not act on until a document is open.
 */
export function readCommands(sources: readonly CommandSource[]): PaletteCommand[] {
	const commands: PaletteCommand[] = [];
	for (const source of sources) {
		const label = source.label.trim();
		if (label === "" || source.action === "") {
			continue;
		}
		commands.push({
			label,
			where: source.where.trim(),
			icon: source.icon,
			action: source.action,
		});
	}
	return commands;
}

/**
 * The commands that match what the reader typed.
 *
 * The match is on the label AND on the group, because "pages" finds the
 * page commands and "protect" finds the protection ones, and a reader who
 * types a word expects every command with that word in it.
 *
 * An empty query matches everything: the palette opens with the whole set,
 * so the reader can see what there is before typing.
 */
export function filterCommands(
	commands: readonly PaletteCommand[],
	query: string,
): PaletteCommand[] {
	const needle = query.trim().toLowerCase();
	if (needle === "") {
		return [...commands];
	}
	const words = needle.split(/\s+/);
	return commands.filter((command) => {
		const haystack = `${command.label} ${command.where}`.toLowerCase();
		return words.every((word) => haystack.includes(word));
	});
}
