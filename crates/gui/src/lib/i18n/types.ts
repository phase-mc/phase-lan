// Dictionary types. The Russian dictionary (`ru/*.ts`) is the source of keys; the other languages must
// repeat all of them: a missing key is a TypeScript error (`npm run check`).

/** Plural by the number `{n}`: the language's `Intl.PluralRules` categories (ru: one/few/many, en: one/other).
 *  `other` is always required — the fallback form. */
export interface Plural {
  zero?: string;
  one?: string;
  two?: string;
  few?: string;
  many?: string;
  other: string;
}

export type Msg = string | Plural;

/** Names of `{placeholders}` in a template string. */
type Names<S extends string> = S extends `${string}{${infer P}}${infer Rest}`
  ? P | Names<Rest>
  : never;

/** Message placeholders: for a string — its own, for a plural — the union of all forms. */
type MsgNames<M> = M extends string
  ? Names<M>
  : M extends Plural
    ? Names<NonNullable<M[keyof M]> & string>
    : never;

/** Parameters of the `t(key, params)` call: no placeholders — no parameters. */
export type ParamsArg<M> = [MsgNames<M>] extends [never]
  ? []
  : [params: { [P in MsgNames<M>]: string | number }];

/** Translation of a dictionary area: the same keys, values are strings or plurals. */
export type Translated<R> = { [K in keyof R]: R[K] extends string ? string : Plural };

export const isPlural = (msg: Msg): msg is Plural => typeof msg !== "string";
