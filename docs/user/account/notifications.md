---
title: Notifications
description: Choose which emails Ployz Cloud sends you, and turn one off from the email itself.
---

Ployz Cloud can email you when something needs you. Open the account menu and click
**Account** to choose which emails you get. Your choices are yours alone, and they apply in
every organization you belong to.

Each kind of email has a row, set to **Email** or **None**:

- **Uptime incidents**: when a check goes down, and when it recovers.
- **Failed GitHub deploys**: when a deploy from a push or pull request fails, or Cloud loses
  track of it partway. The email names the service that failed and the commit GitHub deployed
  when it can tell, and links to the logs. A deploy you start yourself doesn't send one.
- **Failed automatic upgrades**: when an automatic server upgrade fails.

Everything starts at **Email**. A change saves as you pick it.

Ployz Cloud sends all three kinds. An uptime email that couldn't go out within an hour of the
check going down or recovering is skipped, not sent late.

## Turn an email off from the email

Every email ends with a **Turn off** link for its kind, such as **Turn off uptime emails**.
Click it and that row is set to **None**, with no sign-in. Your mail app's own Unsubscribe button does the same. To get the
emails again, set the row back to **Email** on the Account page.

## On a self-hosted Ployz Cloud

A [self-hosted Ployz Cloud](../self-hosting.md#2-configure) sends email only once its operator
sets it up. Until then the rows show your choices but can't be changed, and the page says
**Email isn't set up on this Cloud**.
