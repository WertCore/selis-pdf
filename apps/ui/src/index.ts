import { VERSION } from "@selis/ui-kit";

export { VERSION };

export * from "./platform/index.js";

export function bootstrap(): string {
	return `selis ui ${VERSION}`;
}
