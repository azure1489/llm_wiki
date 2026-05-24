import { describe, expect, it } from "vitest"
import {
  sourceIdentityForPath,
  sourceReferenceIdentity,
  sourceSummarySlugFromIdentity,
} from "./source-identity"

describe("source identity helpers", () => {
  it("keeps raw/sources relative folder context as the source identity", () => {
    expect(
      sourceIdentityForPath("/tmp/project", "/tmp/project/raw/sources/project-a/config.yaml"),
    ).toBe("project-a/config.yaml")
  })

  it("normalizes source references that include raw/sources prefixes", () => {
    expect(sourceReferenceIdentity("raw/sources/project-a/config.yaml")).toBe(
      "project-a/config.yaml",
    )
    expect(sourceReferenceIdentity("/tmp/project/raw/sources/project-a/config.yaml")).toBe(
      "project-a/config.yaml",
    )
  })

  it("keeps legacy basename slugs for root-level sources", () => {
    expect(sourceSummarySlugFromIdentity("config.yaml")).toBe("config")
  })

  it("escapes slug segments so delimiter-containing folders do not collide", () => {
    expect(sourceSummarySlugFromIdentity("a--b/config.yaml")).toMatch(
      /^4-a--b--6-config--[a-z0-9]+$/,
    )
    expect(sourceSummarySlugFromIdentity("a/b/config.yaml")).toMatch(
      /^1-a--1-b--6-config--[a-z0-9]+$/,
    )
    expect(sourceSummarySlugFromIdentity("4-a--b--6-config.yaml")).not.toBe(
      sourceSummarySlugFromIdentity("a--b/config.yaml"),
    )
  })
})
