package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
)

// CategoriesOutput represents the JSON output format for the categories command.
type CategoriesOutput struct {
	CategoryGroups []CategoryGroupInfo `json:"category_groups"`
}

// CategoryGroupInfo represents a category group with its categories.
type CategoryGroupInfo struct {
	ID         string         `json:"id"`
	Name       string         `json:"name"`
	Categories []CategoryInfo `json:"categories"`
}

// CategoryInfo represents a single category's information.
type CategoryInfo struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

// CategoriesCmd retrieves and displays all categories with their IDs.
// Categories are grouped by their category groups.
// If jsonOutput is true, outputs JSON instead of human-readable format.
func CategoriesCmd(client *api.Client, jsonOutput bool) error {
	// Get default budget ID
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	// Get all category groups
	categoryGroups, err := client.GetCategories(budgetID)
	if err != nil {
		return fmt.Errorf("failed to get categories: %w", err)
	}

	// If JSON output requested, marshal and print
	if jsonOutput {
		output := CategoriesOutput{
			CategoryGroups: make([]CategoryGroupInfo, 0),
		}

		for _, group := range categoryGroups {
			// Skip hidden and deleted groups
			if group.Hidden || group.Deleted {
				continue
			}

			// Skip internal master category
			if group.Name == "Internal Master Category" {
				continue
			}

			categoryGroup := CategoryGroupInfo{
				ID:         group.ID,
				Name:       group.Name,
				Categories: make([]CategoryInfo, 0),
			}

			for _, category := range group.Categories {
				// Skip hidden and deleted categories
				if category.Hidden || category.Deleted {
					continue
				}

				categoryGroup.Categories = append(categoryGroup.Categories, CategoryInfo{
					ID:   category.ID,
					Name: category.Name,
				})
			}

			// Only include groups that have categories
			if len(categoryGroup.Categories) > 0 {
				output.CategoryGroups = append(output.CategoryGroups, categoryGroup)
			}
		}

		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		if err := encoder.Encode(output); err != nil {
			return fmt.Errorf("failed to encode JSON: %w", err)
		}
		return nil
	}

	// Human-readable output
	fmt.Printf("Categories:\n\n")

	// Track total categories
	totalCategories := 0

	// Process each category group
	for _, group := range categoryGroups {
		// Skip hidden and deleted groups
		if group.Hidden || group.Deleted {
			continue
		}

		// Skip internal master category
		if group.Name == "Internal Master Category" {
			continue
		}

		// Filter out hidden/deleted categories
		var visibleCategories []*api.Category
		for _, category := range group.Categories {
			if !category.Hidden && !category.Deleted {
				visibleCategories = append(visibleCategories, category)
			}
		}

		// Skip groups with no visible categories
		if len(visibleCategories) == 0 {
			continue
		}

		// Print group header
		fmt.Printf("%s\n", group.Name)
		fmt.Printf("%s\n", strings.Repeat("-", len(group.Name)))

		// Calculate column width for category names
		maxNameLen := 20
		for _, category := range visibleCategories {
			if len(category.Name) > maxNameLen {
				maxNameLen = len(category.Name)
			}
		}

		// Print categories with IDs
		for _, category := range visibleCategories {
			fmt.Printf("  %-*s  %s\n",
				maxNameLen, category.Name, category.ID)
			totalCategories++
		}

		fmt.Println()
	}

	// Print summary
	fmt.Printf("Total: %d categories\n", totalCategories)

	return nil
}
