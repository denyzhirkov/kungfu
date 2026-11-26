variable "region" {
  type    = string
  default = "eu-west-1"
}

variable "env" {
  type        = string
  description = "Deployment environment"

  validation {
    condition     = contains(["dev", "prod"], var.env)
    error_message = "env must be dev or prod."
  }
}

variable "db_password" {
  type      = string
  sensitive = true
}

variable "tags" { default = {} }
