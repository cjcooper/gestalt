# Sample root module for exercising Gestalt. `terraform_data` is built in,
# so init/plan/apply need no providers or credentials.
resource "terraform_data" "example" {
  input = "prod/app"
}

output "where" {
  value = terraform_data.example.output
}
