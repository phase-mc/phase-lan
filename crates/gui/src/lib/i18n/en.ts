// English dictionary: areas live in `en/*.ts`. Must have every key of the Russian one:
// a missing key fails `npm run check`. Phase Client texts live in `src/client/i18n`.

import { lib } from "./en/lib";
import { screens } from "./en/screens";
import { shell } from "./en/shell";
import { ui } from "./en/ui";
import type { ru } from "./ru";
import type { Translated } from "./types";

export const en: Translated<typeof ru> = { ...lib, ...shell, ...ui, ...screens };
