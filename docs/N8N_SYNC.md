# n8n instance link

Hokkabaz reads and writes n8n's own workflow JSON, so a workflow moves between the two unchanged.
Linking an n8n instance adds three things (Plugins screen > n8n tab):

1. **Import**: list the instance's workflows and import one as a local flow.
2. **Push**: Flows screen > import/export menu > *Push to linked n8n instance* creates the workflow there.
3. **Delegation** (opt-in): node types Hokkabaz does not implement run on your instance instead of failing.
   See `N8N_COVERAGE.md` for what is built in.

## Setup

Create an *n8n API* credential (Plugins > Credentials): API key from n8n (Settings > n8n API) and the base URL
(`https://host` or `https://host/api/v1`). Pick it in the n8n tab. *Test connection* checks it.

## Credentials on import

n8n's API cannot list credentials or read their values. On import each node credential reference is re-pointed at a
local credential of the same **type and name**; the rest keep their n8n reference and are listed in the import message.
Create them under Plugins > Credentials and choose them in the node panel.

## How delegation works

n8n has no "run one node" API. For each delegated node Hokkabaz creates a short-lived workflow
`Webhook -> Code (items) -> <the node> -> Respond to Webhook` named `hokkabaz-delegate-*`, activates it, calls its
production webhook with the node's input items, then deactivates and deletes it.

Limits (a node that needs more should be ported to a built-in or a plugin):

* first input and first output only (no Merge/Switch style fan-in/out);
* binary data is not sent or returned;
* expressions that read other nodes (`$('Node')`) do not resolve remotely; `$json` does;
* the node's credentials must exist on the instance (imported workflows already carry the right references);
* items leave the device, hence the switch is off by default and the UI says so;
* the instance must be reachable and the webhook URL (`WEBHOOK_URL`) configured correctly.

## Where it lives

`hk-integrations::n8n_remote` (client, credential mapping, `RemoteNodeRunner`), `hk-flow::hooks::RemoteNodeRunner`
(the executor consults it only for unregistered node types), `hk-ffi::n8n_sync` (link settings), Flutter `plugins/n8n_tab.dart`.
