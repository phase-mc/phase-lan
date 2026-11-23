// Русский словарь целиком: области лежат в `ru/*.ts`. Это источник ключей для всех языков.
// Тексты окна Phase Client живут отдельно, в `src/client/i18n`.

import { lib } from "./ru/lib";
import { screens } from "./ru/screens";
import { shell } from "./ru/shell";
import { ui } from "./ru/ui";

export const ru = { ...lib, ...shell, ...ui, ...screens } as const;
