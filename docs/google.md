# Google Calendar

calm can read a Google account through the Calendar API instead of a secret iCal address.
You get every calendar in your list (with Google's names and colours), calendars shared with
you as free/busy only, and incremental sync: after the first fetch Google sends only what
changed, so calm checks every five minutes.

Access is read-only (`calendar.readonly`). calm talks to Google directly. Tokens stay on your
machine in `~/.config/calm/secrets/` (mode 600).

## One-time setup: your own OAuth client

Google requires an OAuth client to sign in. For personal use, create your own. It takes about
five minutes. Console menus get renamed now and then, so look for the closest match.

1. Open [console.cloud.google.com](https://console.cloud.google.com/) and create a project
   (e.g. "calm").
2. **APIs & Services → Library**: enable **Google Calendar API**.
3. **Google Auth Platform → Branding** (or "OAuth consent screen"): app name "calm", your
   address as support and developer contact. Audience: **External**.
4. **Audience → Publishing status**: choose **In production**. In "Testing", Google expires
   sign-ins after 7 days. In production but unverified, you'll see a "Google hasn't verified
   this app" warning when you sign in: choose *Advanced → Go to calm*. That's expected for a
   client only you use.
5. **Clients → Create client**: type **Desktop app**, name "calm". **Download JSON**.
6. Save it as `~/.config/calm/secrets/google-client.json`, then
   `chmod 600 ~/.config/calm/secrets/google-client.json`.

## Sign in

```sh
calm google login you@gmail.com
```

Your browser opens Google's consent page. Afterwards calm saves the sign-in and prints the line
to add to `calm.kdl`:

```kdl
source "Google" google="you@gmail.com"
```

If you were syncing the same calendar by its secret iCal address, remove that `source`, or
you'll see everything twice.

## What you'll see

- **Calendars**: the ones ticked in Google Calendar, in Google's order and colours. Your own
  calendar is named after the source (`"Google"` above). Rename it with the source's name, or
  restyle any of them with a `calendar` line using the same name:
  ```kdl
  calendar "Google" color="clay" demand=0.5
  calendar "Family" color="sage" demand=0.3
  ```
- **Free/busy calendars** (shared with you as "See only free/busy"): "Busy" blocks in that calendar's colour, from
  a month back to six months ahead. That's all Google shares for them.
- **Recurring events** are expanded by calm with the same code as iCal feeds, with moved and
  cancelled instances honoured.

## When something goes wrong

The top bar says what failed. A failed sync never removes cached events.

- **"not signed in"** or **"sign-in expired"**: run `calm google login you@gmail.com` again.
- **"no refresh token"**: Google only hands one out on first consent. Remove calm at
  [myaccount.google.com/permissions](https://myaccount.google.com/permissions) and sign in again.
- **"Google 403"**: check that the Calendar API is enabled in your project.

To sign out, delete `~/.config/calm/secrets/google-you@gmail.com.json` and remove calm at
[myaccount.google.com/permissions](https://myaccount.google.com/permissions).
