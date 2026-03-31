package cmd

import "strings"

// splitArgs separates positional arguments from flags, allowing flags to appear
// after positional arguments (which flag.FlagSet doesn't natively support).
// Flags with separate values (--flag value) are kept together; flags with
// inline values (--flag=value) are treated as a single arg.
func splitArgs(args []string) (flagArgs, positional []string) {
	for i := 0; i < len(args); i++ {
		arg := args[i]
		if strings.HasPrefix(arg, "-") {
			flagArgs = append(flagArgs, arg)
			if !strings.Contains(arg, "=") && i+1 < len(args) && !strings.HasPrefix(args[i+1], "-") {
				i++
				flagArgs = append(flagArgs, args[i])
			}
		} else {
			positional = append(positional, arg)
		}
	}
	return flagArgs, positional
}
