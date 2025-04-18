#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdbool.h>

/**
* While a proper testing harness would be great this will at least show what we expect outcomes to be
* The only abnormal behavior that I know of now is related to strings longer than 255 bytes (which I believe
* the codebase filters out already AND that the domain can be "" (empty) if the user provides a specific malformed
* username (btcaddress.lightningaddress@.workername).
*/

char* str_dup(const char* s) {
    if (!s) return NULL;
    char* d = malloc(strlen(s) + 1);
    if (d) strcpy(d, s);
    return d;
}

// I normally split struct to header file, but no header for tests is cleaner
typedef struct {
    char* username;      // BTC address part
    char* lightning_id;  // Lightning ID part
    char* domain;        // Domain part
    char* worker_suffix; // Worker name suffix
} parse_result_t;

parse_result_t parse_workername(const char* workername) {
    parse_result_t result = {0};
    char* full_username = str_dup(workername);
    char* tmp = str_dup(full_username);

    // First, split by periods to separate potential worker suffix
    result.username = strsep(&tmp, ".");
    if (!tmp) {
        // No periods in the workername, simple case
        // Just use the whole thing as username
    } else {
        // We have at least one period
        // Check for @ in the remaining string to identify lightning ID
        char* at_tmp = tmp;
        char* at_part = strchr(at_tmp, '@');

        if (at_part) {
            // Format is btcaddress.lightning@domain.workername
            // Reset and reparse
            tmp = str_dup(full_username);

            // First part is the BTC address (before first period)
            result.username = strsep(&tmp, ".");

            if (tmp) {
                // Get the lightning part (between first period and @)
                result.lightning_id = strsep(&tmp, "@");

                if (tmp) {
                    // The domain is everything between @ and next period (or end)
                    result.domain = strsep(&tmp, ".");

                    // If anything remains after the last period, it's the worker suffix
                    result.worker_suffix = tmp;
                }
            }
        } else {
            // Format is just username.workername
            // worker_suffix is already set correctly by the first strsep
            result.worker_suffix = tmp;
        }
    }

    // Final username after all logic
    if (!result.username || !strlen(result.username))
        result.username = str_dup(full_username);
    return result;
}

// Function to print parsing results
void print_parse_result(const char* workername, parse_result_t result) {
    printf("\nParsing results for: \"%s\"\n", workername);
    printf("----------------------------------------\n");
    printf("BTC Address: %s\n", result.username ? result.username : "NULL");
    printf("Lightning ID: %s\n", result.lightning_id ? result.lightning_id : "NULL");
    printf("Domain: %s\n", result.domain ? result.domain : "NULL");
    printf("Worker Name: %s\n", result.worker_suffix ? result.worker_suffix : "NULL");
    printf("----------------------------------------\n");
}

bool str_equal(const char* a, const char* b) {
    if (a == NULL && b == NULL) return true;  // Both NULL, considered equal
    if (a == NULL || b == NULL) return false; // One NULL, one not NULL, not equal
    return strcmp(a, b) == 0;                 // Both non-NULL, compare content
}

// Test function to demonstrate the parsing behavior
int test_predefined_examples() {
    const char* test_cases[] = {
        "user1",
        "user1.worker1",
        "btcaddress.lightning@domain.worker1",
        "btcaddress.lightning@domain",
        "btcaddress.lightning@",
        "user1.worker1.rig2",
        "user1.worker@rig2",
        "1abc123def.lnid@example.com.worker1",
        "btc.lightning@domain@extra.worker",
        "bc1address.lightning@domain.worker.rig1.gpu2"
    };

    const parse_result_t expected[] = {
        { strdup("user1"), NULL, NULL, NULL },
        { strdup("user1"), NULL, NULL, strdup("worker1") },
        { strdup("btcaddress"), strdup("lightning"), strdup("domain"), strdup("worker1") },
        { strdup("btcaddress"), strdup("lightning"), strdup("domain"), NULL },
        { strdup("btcaddress"), strdup("lightning"), strdup(""), NULL },
        { strdup("user1"), NULL, NULL, strdup("worker1.rig2") },
        { strdup("user1"), strdup("worker"), strdup("rig2"), NULL },
        { strdup("1abc123def"), strdup("lnid"), strdup("example"), strdup("com.worker1") },
        { strdup("btc"), strdup("lightning"), strdup("domain@extra"), strdup("worker") },
        { strdup("bc1address"), strdup("lightning"), strdup("domain"), strdup("worker.rig1.gpu2") }
    };

    for (int i = 0; i < sizeof(test_cases) / sizeof(test_cases[0]); i++) {
        parse_result_t result = parse_workername(test_cases[i]);
        //print_parse_result(test_cases[i], result);
        if (!str_equal(result.username, expected[i].username)) {
            printf("Unexpected value for username while parsing %s: expected %s found %s\n", test_cases[i], expected[i].username, result.username);
            return 1;
        }
        if (!str_equal(result.lightning_id, expected[i].lightning_id)) {
            printf("Unexpected value for lightning address while parsing %s: expected %s found %s\n", test_cases[i], expected[i].lightning_id, result.lightning_id);
            return 1;
        }
        if (!str_equal(result.domain, expected[i].domain)) {
            printf("Unexpected value for lightning domain while parsing %s: expected %s found %s\n", test_cases[i], expected[i].domain, result.domain);
            return 1;
        }
        if (!str_equal(result.worker_suffix, expected[i].worker_suffix)) {
            printf("Unexpected value for workername while parsing %s: expected %s found %s\n", test_cases[i], expected[i].worker_suffix, result.worker_suffix);
            return 1;
        }
    }
    return 0;
}

int main() {
    char workername[256];

    return test_predefined_examples();
}