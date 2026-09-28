import { VERSION } from "@selis/ui-kit";

export { VERSION };

export * from "./i18n/index.js";
export * from "./platform/index.js";
export * from "./viewer/index.js";

export function bootstrap(): string {
	return `selis ui ${VERSION}`;
}
