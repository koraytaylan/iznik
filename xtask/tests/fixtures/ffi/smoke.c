/* A Swift developer's first afternoon, in C.
 *
 * Everything an application does on the way to a pane it can draw: make a
 * client, hold a host, make a session on it, attach to the pane that comes
 * with it, type a line, take the bytes back, return the credit they cost, let
 * the pane go, end the client. Exits zero when all of that happened, and
 * prints what went wrong when it did not.
 *
 * Its one argument is the alias of the host, which the case that runs it
 * points at a daemon on this machine. What it needs besides that it takes
 * from the directory it is run in: `command.bin`, the session command encoded
 * as the protocol encodes it, and `runtime`, which is where iznik keeps its
 * own files.
 */

/* For `nanosleep`, which C11 on its own does not declare. */
#define _POSIX_C_SOURCE 200809L

#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "iznik.h"

/* How long anything here waits for something that should happen at once. */
#define PATIENCE_SECONDS 10

/* How often it looks while waiting. */
#define LOOK_NANOSECONDS 5000000L

/* The pane a session comes with. */
#define PANE 1

/* What is typed, and what is looked for in what comes back. */
static const char TYPED[] = "echo smoke-42\n";
static const char MARKER[] = "smoke-42";

/* What the callbacks have seen. The application's own thread reads these and
 * iznik's writes them, which is what makes them atomic. */
static atomic_bool answered = false;
static atomic_bool drew = false;
static atomic_bool echoed = false;
static atomic_size_t taken = 0;

/* Our command's number, set before the callback can see it. */
static uint64_t asked = 0;

/* Whether a run of bytes holds another. */
static bool holds(const uint8_t *held, size_t length, const char *wanted) {
    size_t wanted_length = strlen(wanted);
    if (held == NULL || length < wanted_length) {
        return false;
    }
    for (size_t at = 0; at + wanted_length <= length; at++) {
        if (memcmp(held + at, wanted, wanted_length) == 0) {
            return true;
        }
    }
    return false;
}

/* Everything the host says that is not a pane's own bytes. */
static void heard(const iznik_event *event, void *context) {
    (void)context;
    if (event == NULL) {
        return;
    }
    if (event->kind == IZNIK_EVENT_KIND_COMMAND_RESULT && event->command_id == asked) {
        atomic_store(&answered, true);
    }
}

/* The pane's screen, which must arrive before any of its output. */
static void screened(void *context, uint64_t sequence, uint16_t columns, uint16_t rows,
                     const uint8_t *bytes, size_t length) {
    (void)context;
    (void)sequence;
    (void)bytes;
    (void)length;
    if (columns > 0 && rows > 0) {
        atomic_store(&drew, true);
    }
}

/* The pane's own bytes, which is the one place iznik does not copy. */
static void printed(void *context, const uint8_t *bytes, size_t length) {
    (void)context;
    atomic_fetch_add(&taken, length);
    if (!atomic_load(&drew)) {
        /* Output before a screen is the one thing the boundary promises will
         * not happen; leaving `echoed` false is what fails the run. */
        return;
    }
    if (holds(bytes, length, MARKER)) {
        atomic_store(&echoed, true);
    }
}

/* Waits for one of the flags above, and says whether it came. */
static bool await(atomic_bool *flag) {
    struct timespec look = {0, LOOK_NANOSECONDS};
    long looks = (PATIENCE_SECONDS * 1000000000L) / LOOK_NANOSECONDS;
    for (long at = 0; at < looks; at++) {
        if (atomic_load(flag)) {
            return true;
        }
        nanosleep(&look, NULL);
    }
    return false;
}

/* Says what went wrong, with whatever iznik put in the error. */
static int refused(const char *what, const iznik_error *error) {
    const char *said = (error != NULL && error->message != NULL) ? error->message : "";
    fprintf(stderr, "%s: %s\n", what, said);
    return 1;
}

/* Reads a file whole, or answers NULL. */
static uint8_t *read_whole(const char *path, size_t *length) {
    FILE *held = fopen(path, "rb");
    if (held == NULL) {
        return NULL;
    }
    if (fseek(held, 0, SEEK_END) != 0) {
        fclose(held);
        return NULL;
    }
    long size = ftell(held);
    if (size < 0 || fseek(held, 0, SEEK_SET) != 0) {
        fclose(held);
        return NULL;
    }
    uint8_t *bytes = malloc((size_t)size);
    if (bytes == NULL) {
        fclose(held);
        return NULL;
    }
    size_t read = fread(bytes, 1, (size_t)size, held);
    fclose(held);
    if (read != (size_t)size) {
        free(bytes);
        return NULL;
    }
    *length = read;
    return bytes;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: smoke <alias>\n");
        return 2;
    }
    const char *alias = argv[1];
    iznik_error error = {0, IZNIK_LAYER_CLIENT, NULL};

    iznik_configuration configuration = {"./runtime", NULL, NULL, NULL};
    iznik_client *client = iznik_client_new(&configuration, &error);
    if (client == NULL) {
        return refused("no client", &error);
    }
    iznik_set_event_callback(client, heard, NULL);
    if (iznik_host_add(client, alias, &error) != IZNIK_OK) {
        return refused("the host was not taken", &error);
    }

    size_t length = 0;
    uint8_t *command = read_whole("./command.bin", &length);
    if (command == NULL) {
        fprintf(stderr, "no command.bin beside this program\n");
        return 1;
    }
    if (iznik_command(client, alias, command, length, &asked, &error) != IZNIK_OK) {
        free(command);
        return refused("the session was not asked for", &error);
    }
    free(command);
    if (!await(&answered)) {
        fprintf(stderr, "the host never answered the command\n");
        return 1;
    }

    iznik_pane_callbacks callbacks = {printed, screened, NULL, NULL};
    if (iznik_pane_attach(client, alias, PANE, callbacks, NULL, &error) != IZNIK_OK) {
        return refused("the pane was not taken", &error);
    }
    if (!await(&drew)) {
        fprintf(stderr, "no screen arrived for the pane\n");
        return 1;
    }
    if (iznik_pane_input(client, alias, PANE, (const uint8_t *)TYPED, strlen(TYPED), &error) !=
        IZNIK_OK) {
        return refused("the line was not taken", &error);
    }
    if (!await(&echoed)) {
        fprintf(stderr, "the line never came back\n");
        return 1;
    }

    size_t owed = atomic_load(&taken);
    if (iznik_pane_credit(client, alias, PANE, (uint32_t)owed, &error) != IZNIK_OK) {
        return refused("the credit was not returned", &error);
    }
    if (iznik_pane_detach(client, alias, PANE, &error) != IZNIK_OK) {
        return refused("the pane was not let go", &error);
    }
    iznik_client_free(client);
    printf("smoke: %zu bytes, screen first, credit returned\n", owed);
    return 0;
}
