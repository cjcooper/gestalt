# Sample root module for exercising Gestalt. `terraform_data` is built in,
# so init/plan/apply need no providers or credentials. The variables are
# fed from the .gestalt.env files between examples/ and this folder.
variable "owner" {
  type    = string
  default = "nobody"
}

variable "environment" {
  type    = string
  default = "none"
}

variable "label" {
  type    = string
  default = ""
}

resource "terraform_data" "example" {
  input = {
    module      = "dev/network"
    owner       = var.owner
    environment = var.environment
    label       = var.label
  }
}

output "example" {
  value = terraform_data.example.output
}
