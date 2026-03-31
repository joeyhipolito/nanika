package cmd

import (
	"fmt"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// CalendarListCmd handles "gmail calendar list [--limit N] --account <alias> [--json]".
func CalendarListCmd(cfg *config.Config, account string, limit int, jsonOutput bool) error {
	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	events, err := client.ListUpcomingEvents(limit)
	if err != nil {
		return fmt.Errorf("list events: %w", err)
	}

	if len(events) == 0 {
		if jsonOutput {
			fmt.Println("[]")
			return nil
		}
		fmt.Println("No upcoming events.")
		return nil
	}

	if jsonOutput {
		return printJSON(events)
	}
	printCalendarEvents(events)
	return nil
}

// CalendarCreateCmd handles "gmail calendar create --summary <title> --start <RFC3339> --end <RFC3339> ... --account <alias> [--json]".
func CalendarCreateCmd(cfg *config.Config, params api.CreateEventParams, account string, jsonOutput bool) error {
	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	ev, err := client.CreateEvent(params)
	if err != nil {
		return fmt.Errorf("create event: %w", err)
	}

	if jsonOutput {
		return printJSON(ev)
	}

	fmt.Printf("Event created: %s\n", ev.Summary)
	fmt.Printf("Start: %s\n", formatCalendarTime(ev.Start, ev.AllDay))
	fmt.Printf("End:   %s\n", formatCalendarTime(ev.End, ev.AllDay))
	if ev.HtmlLink != "" {
		fmt.Printf("Link:  %s\n", ev.HtmlLink)
	}
	return nil
}

// CalendarAvailableCmd handles "gmail calendar available --start <RFC3339> --end <RFC3339> --account <alias> [--json]".
func CalendarAvailableCmd(cfg *config.Config, account string, start, end time.Time, jsonOutput bool) error {
	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	fb, err := client.CheckAvailability(start, end)
	if err != nil {
		return fmt.Errorf("check availability: %w", err)
	}

	if jsonOutput {
		return printJSON(fb)
	}

	printFreeBusy(fb)
	return nil
}

func printCalendarEvents(events []api.CalendarEvent) {
	for _, ev := range events {
		start := formatCalendarTime(ev.Start, ev.AllDay)
		fmt.Printf("[%s] %s\n", start, ev.Summary)
		if ev.Location != "" {
			fmt.Printf("  Location: %s\n", ev.Location)
		}
		if len(ev.Attendees) > 0 {
			fmt.Printf("  Attendees: %s\n", strings.Join(ev.Attendees, ", "))
		}
	}
}

func printFreeBusy(fb *api.FreeBusy) {
	startStr := formatCalendarTime(fb.TimeMin, false)
	endStr := formatCalendarTime(fb.TimeMax, false)
	if fb.Available {
		fmt.Printf("Available between %s and %s\n", startStr, endStr)
		return
	}
	fmt.Printf("Busy between %s and %s:\n", startStr, endStr)
	for _, slot := range fb.BusySlots {
		fmt.Printf("  • %s – %s\n",
			formatCalendarTime(slot.Start, false),
			formatCalendarTime(slot.End, false),
		)
	}
}

// formatCalendarTime formats a calendar time string for human display.
// allDay events use YYYY-MM-DD; timed events use RFC3339 converted to local time.
func formatCalendarTime(t string, allDay bool) string {
	if allDay || t == "" {
		return t
	}
	parsed, err := time.Parse(time.RFC3339, t)
	if err != nil {
		return t
	}
	return parsed.Local().Format("Mon Jan 2 3:04 PM")
}
