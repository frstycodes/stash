---
name: store-release
description: Submit the latest Stash GitHub release to the Microsoft Store through Partner Center in the Claude desktop app's built-in browser. Use when the user says "/store-release", "publish to the Store", "update the Store version", or after a new version tag is released.
---

# Publish the latest release to the Microsoft Store

Stash's Partner Center account has no Entra tenant, so the release workflow can't submit to the Store by itself. This skill does it in the built-in browser (`mcp__Claude_Browser__*`), where the user stays signed in to Partner Center.

**Facts:**
- App: "Stash: Downloads stack", product id `9PMVDZ3LZ490`.
- Overview page: `https://partner.microsoft.com/en-us/dashboard/products/9PMVDZ3LZ490/overview`.
- Each release attaches `Stash-<version>.msixbundle` (x64 + Arm64) and `SHA256SUMS.txt`.

## Steps

1. **Find the release.**
   - Run `gh release list --limit 1` and `gh release view <tag> --json assets`, in `native/`.
   - Download the bundle and the checksums into the scratchpad with `gh release download <tag> -p "*.msixbundle" -p SHA256SUMS.txt -D <dir>`, then check the bundle's SHA-256 against `SHA256SUMS.txt`. Stop if it doesn't match.
   - Copy the bundle to `%USERPROFILE%\Downloads\` so it's easy to pick in the upload dialog.

2. **Open the app in Partner Center.**
   - Open the overview page with `preview_start` (`url`), then wait. Partner Center takes 5–15 seconds to render, so poll with `find` or `get_page_text`.
   - If it lands on `login.microsoftonline.com`, ask the user to sign in in the browser pane, and say where it is: Ctrl+Shift+B shows it. Wait for them.

3. **Start or reuse a draft.**
   - Under **Product release**, a "Product update: In draft" block means a draft exists, so reuse it.
   - Otherwise select **Start update**; this creates a draft, which is safe and can be deleted.
   - If a submission is **in certification**, stop and tell the user: a new one can't start until it finishes, unless they cancel it.

4. **Packages.**
   - Open the draft's **Packages** link (`.../submissions/<id>/packages`).
   - First read the page (`get_page_text`). If `Stash-<version>.msixbundle` is already listed (the user may have uploaded it already), don't ask again; go straight to the next bullet.
   - Otherwise ask the user to select **browse your files** and pick `Downloads\Stash-<version>.msixbundle`, or drag it onto the drop area.
   - This step is manual on purpose. The browser tools can't fill a file dialog, and the built-in browser blocks pages from reading files served from this PC, so don't try to work around it.
   - When it's uploaded and shows **Validated**:
     - Partner Center marks older packages for removal by itself ("This package will be removed after you save…"); leave those as they are.
     - Leave the device family checkboxes as they are.
     - Select **Save** at the bottom. It's an `input.btn-primary` with the value "Save", which `find` can miss, so click it through `javascript_tool`.
     - A warning that runFullTrust "requires approval" is expected. Stash is a desktop app, and the reason is in Submission options > Notes for certification.

5. **Store listing (only when it changed).**
   - Compare the live listing (overview > Store listings > English) with `native/packaging/store/listing.md`.
   - If they differ, offer to update the Description, Short description, Product features and Search terms from that file, using `form_input`, then **Save**.
   - Don't change screenshots unless the user provides new ones.

6. **Submit, only with the user's OK.**
   - Go back to the overview and check every section reads Complete, Unchanged or Validated.
   - Tell the user what will be submitted (the version, packages replaced, whether the listing changed), and ask before selecting **Submit for certification**. This is an outward-facing action; never do it without a clear yes in this conversation.
   - After submitting, report the status Partner Center shows. Certification usually takes 1–3 days.

## Page details (learned on the 0.5.0 run)

- **Store listing (English):**
  - URL: `.../submissions/<id>/listings?languageid=4`.
  - Fields by id: `description-required`, `shortDescription`, `releaseNotes` ("What's new"), `feature-0`…`feature-8` (nine feature lines; there's no control to add more), `shortTitle`, `devStudio`. There's no search-terms field.
  - Set values through the native `value` setter plus `input`, `change` and `blur` events, or the page doesn't notice.
- **Submission options:**
  - URL: `.../submissions/<id>/options`.
  - Its one textarea is the restricted-capability justification, at most **500** characters. A longer note won't save and leaves the section "Incomplete".
  - Keep the note in step with `listing.md`'s "Notes for certification".
- **Clicking:** the pane's zoom can change mid-run. Click buttons by `find` reference, not by screenshot coordinates.
- **After a section saves,** Partner Center returns to the overview. Re-read it to see each section's status.

## Notes

- Never sign out, change account settings, or touch payout, tax or user management.
- If the page layout changed and a step's control can't be found, describe what's on screen and ask the user rather than clicking around.
- A draft left half-done is harmless; **Delete submission** removes it.
