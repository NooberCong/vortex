import { describe, expect, it } from "vitest";

import { describe as describeResponse } from "@/src/capture";
import { cookieHeader, filenameFromDisposition, headers } from "@/src/envelope";

describe("capturing what the browser sent", () => {
  it("keeps header order and case, because the engine replays them verbatim", () => {
    const list = [
      { name: "User-Agent", value: "Mozilla/5.0" },
      { name: "Referer", value: "https://example.com/page" },
      { name: "Sec-Fetch-Dest", value: "document" },
    ];
    expect(headers(list)).toEqual([
      ["User-Agent", "Mozilla/5.0"],
      ["Referer", "https://example.com/page"],
      ["Sec-Fetch-Dest", "document"],
    ]);
  });

  it("moves the cookie into its own field rather than leaving it among the headers", () => {
    // The protocol gives cookies a dedicated field because the daemon keeps credentials
    // in memory and never writes them to the database, the logs, or a `.vxpart.meta`. A
    // cookie hiding in `headers` would be persisted like any other header and defeat that
    // silently — which is the worst way for a security boundary to fail.
    const list = [
      { name: "Cookie", value: "session=abc; theme=dark" },
      { name: "Accept", value: "*/*" },
    ];
    expect(headers(list).map(([name]) => name)).toEqual(["Accept"]);
    expect(cookieHeader(list)).toBe("session=abc; theme=dark");
    expect(cookieHeader([{ name: "cookie", value: "" }])).toBeUndefined();
  });

  it("reads a UTF-8 filename in preference to the ASCII fallback beside it", () => {
    expect(
      filenameFromDisposition(
        "attachment; filename=\"Rene.pdf\"; filename*=UTF-8''Ren%C3%A9%20Descartes.pdf",
      ),
    ).toBe("René Descartes.pdf");
    expect(filenameFromDisposition('attachment; filename="report.zip"')).toBe("report.zip");
    expect(filenameFromDisposition("attachment; filename=report.zip")).toBe("report.zip");
    expect(filenameFromDisposition("inline")).toBeUndefined();
    expect(filenameFromDisposition(undefined)).toBeUndefined();
    // A broken escape must fall back rather than throw: the alternative is losing the
    // whole download to a malformed header.
    expect(filenameFromDisposition("attachment; filename*=UTF-8''%E0%A4%A; filename=x.bin")).toBe(
      "x.bin",
    );
  });

  it("reads size and type off the response, and refuses to invent either", () => {
    const observed = describeResponse([
      { name: "Content-Type", value: "video/mp4; codecs=\"avc1.42E01E\"" },
      { name: "Content-Length", value: "1048576" },
      { name: "Content-Disposition", value: "attachment; filename=clip.mp4" },
    ]);
    expect(observed.mimeType).toBe("video/mp4");
    expect(observed.contentLength).toBe(1048576);
    expect(observed.filenameHint).toBe("clip.mp4");

    // A chunked response has no length, and guessing one would make the New Download
    // sheet state a size it cannot know.
    const chunked = describeResponse([{ name: "Content-Type", value: "application/zip" }]);
    expect(chunked.contentLength).toBeUndefined();
    expect(describeResponse([{ name: "Content-Length", value: "nonsense" }]).contentLength)
      .toBeUndefined();
  });
});
