/**
 * The command palette's own logic.
 *
 * The palette is a view over the ribbon, so the two things worth testing are
 * which buttons count as commands and the matching of what the reader typed
 * against them. The rest - opening the dialog, moving the selection, running
 * the command - is the browser's own behaviour.
 *
 * There is no DOM in here on purpose: the shell reads the ribbon's buttons
 * and hands the palette plain data, so these tests run in Node (ADR-P0044).
 */

import { describe, expect, it } from "vitest";
import {
	type CommandSource,
	type PaletteCommand,
	filterCommands,
	readCommands,
} from "./palette.js";

const SOURCES: CommandSource[] = [
	{ label: "Fit width", where: "View", icon: "fit-width", action: "fit-width" },
	{ label: "Fit page", where: "View", icon: "fit-page", action: "fit-page" },
	{ label: "Watermark", where: "Protect", icon: "watermark", action: "section:watermark" },
	{ label: "", where: "View", icon: null, action: "section:empty" },
	{ label: "Nothing to do", where: "View", icon: null, action: "" },
];

describe("the commands the palette reads", () => {
	it("are the buttons that carry an action and a name", () => {
		const commands = readCommands(SOURCES);
		expect(commands.map((command) => command.action)).toEqual([
			"fit-width",
			"fit-page",
			"section:watermark",
		]);
		const first = commands[0];
		expect(first?.label).toBe("Fit width");
		expect(first?.where).toBe("View");
		expect(first?.icon).toBe("fit-width");
	});

	it("skip a button with no label", () => {
		const commands = readCommands(SOURCES);
		expect(commands.some((command) => command.action === "section:empty")).toBe(false);
	});

	it("skip a button with no action", () => {
		const commands = readCommands(SOURCES);
		expect(commands.some((command) => command.label === "Nothing to do")).toBe(false);
	});
});

describe("the commands that match what the reader typed", () => {
	const commands: PaletteCommand[] = [
		{ label: "Fit width", where: "View", icon: null, action: "fit-width" },
		{ label: "Fit page", where: "View", icon: null, action: "fit-page" },
		{ label: "Watermark", where: "Protect", icon: null, action: "section:watermark" },
		{ label: "Bates numbers", where: "Protect", icon: null, action: "section:bates" },
	];

	it("are all of them when nothing was typed", () => {
		expect(filterCommands(commands, "")).toHaveLength(4);
		expect(filterCommands(commands, "   ")).toHaveLength(4);
	});

	it("match on the label", () => {
		expect(filterCommands(commands, "fit").map((c) => c.action)).toEqual(["fit-width", "fit-page"]);
	});

	it("match on the group as well", () => {
		expect(filterCommands(commands, "protect").map((c) => c.action)).toEqual([
			"section:watermark",
			"section:bates",
		]);
	});

	it("need every word to match", () => {
		expect(filterCommands(commands, "fit protect")).toHaveLength(0);
		expect(filterCommands(commands, "fit view").map((c) => c.action)).toEqual([
			"fit-width",
			"fit-page",
		]);
	});

	it("ignore the case", () => {
		expect(filterCommands(commands, "WATERMARK").map((c) => c.action)).toEqual([
			"section:watermark",
		]);
	});

	it("find nothing when nothing matches", () => {
		expect(filterCommands(commands, "zzz")).toHaveLength(0);
	});
});
