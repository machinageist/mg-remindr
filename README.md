# mg-todo

`mg-todo` is a bootstrap for a planned independent todo authority. The planned
scope includes todos, projects, tags, relationships, lifecycle, versions,
recurrence, reminders, and delivery records; those capabilities are not all
implemented in this bootstrap.

The current bootstrap provides domain/configuration boundaries and a deferred
storage boundary. It deliberately contains no migrations, live database writes,
import, compatibility adapter, or agenda integration. PostgreSQL configuration
is local-only and its integration-test path is opt-in.
