// Invite links: what the host copies and what the guest may paste into "Join".
import { describe, expect, it } from "vitest";
import { codeFromInput, inviteUrl, SITE_ORIGIN } from "./invite";

describe("ссылка-приглашение", () => {
  it("одна ссылка на все языки, код во фрагменте", () => {
    expect(inviteUrl("K7F-2QX")).toBe(`${SITE_ORIGIN}/join/#K7F-2QX`);
    expect(inviteUrl("k7f2qx")).toBe(`${SITE_ORIGIN}/join/#K7F-2QX`);
  });

  it("в ссылку попадает только короткий код", () => {
    expect(inviteUrl("")).toBeNull();
    expect(inviteUrl("vanilla-crew")).toBeNull();
    expect(inviteUrl("endpointaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")).toBeNull();
  });
});

describe("что гость вставляет в «Подключиться»", () => {
  it("код в любом виде", () => {
    expect(codeFromInput("K7F-2QX")).toBe("K7F-2QX");
    expect(codeFromInput("  k7f 2qx ")).toBe("K7F-2QX");
  });

  it("ссылку с сайта — на любом языке и без языка", () => {
    for (const path of ["/ru/join/", "/en/join/", "/tr/join/", "/join/", "/ru/join"]) {
      expect(codeFromInput(`${SITE_ORIGIN}${path}#K7F-2QX`), path).toBe("K7F-2QX");
    }
    expect(codeFromInput(`${SITE_ORIGIN}/ru/join/#k7f2qx`)).toBe("K7F-2QX");
    expect(codeFromInput(`${SITE_ORIGIN}/ru/join/#K7F%2D2QX`)).toBe("K7F-2QX");
  });

  it("ссылку phase://", () => {
    expect(codeFromInput("phase://join/K7F-2QX")).toBe("K7F-2QX");
    expect(codeFromInput("PHASE://JOIN/k7f2qx/")).toBe("K7F-2QX");
  });

  it("чужие и испорченные ссылки — не код", () => {
    for (const text of [
      "https://evil.example/ru/join/#K7F-2QX", // another site
      "http://phase.yeskendir.com/ru/join/#K7F-2QX", // not https
      `${SITE_ORIGIN}/ru/lan/#K7F-2QX`, // another page
      `${SITE_ORIGIN}/de/join/#K7F-2QX`, // no such language
      `${SITE_ORIGIN}/ru/join/?code=K7F-2QX`, // the code is never in the query
      `${SITE_ORIGIN}/ru/join/`, // no code
      `${SITE_ORIGIN}/ru/join/#K7F-2QI`, // no I in the codes
      `${SITE_ORIGIN}/ru/join/#%E0%A4%A`, // broken encoding
      "phase://join/K7F-2QX?t=x",
      "phase://join/a/b",
      "endpointaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
      "",
    ]) {
      expect(codeFromInput(text), text).toBeNull();
    }
  });
});
